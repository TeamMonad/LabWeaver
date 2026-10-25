import { reactive, ref, watch, type Ref } from 'vue'
import { createEnvironmentTemplateRelease, listProjectTemplateImages } from '@/generated/contracts'
import type {
  CreateEnvironmentTemplateReleaseRequestSchema,
  EnvironmentSpec,
  EnvironmentTemplateReleaseViewSchema,
  PlatformImageCatalogSchemaPlatformImageEntry,
} from '@/generated/contracts'
import { diagnosticFromError, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, newUuidV7 } from '@/utils/format'
import { clearPendingCommand, readPendingCommand, writePendingCommand } from '@/utils/pendingCommands'

export type TemplateImage = PlatformImageCatalogSchemaPlatformImageEntry

const GIB = 1024 ** 3
const DAY_MS = 24 * 60 * 60 * 1000
/** The contract's limits on the manual review fields. */
export const MANUAL_TEMPLATE_LIMITS = { reason: 500, description: 4000, templateFamilyId: 128 } as const
export const TEMPLATE_FAMILY_PATTERN = /^[A-Za-z0-9._-]+$/

/**
 * Everything a reviewer fills in for one manual Work template. Deployment
 * bindings have no read contract, so the reviewer enters the values the
 * platform administrator provides (or copies them from an earlier manual
 * version of the same template).
 */
export interface ManualWorkTemplateDraft {
  imageId: string
  name: string
  description: string
  templateFamilyId: string
  reason: string
  cpuMillicores: number
  memoryGiB: number
  storageGiB: number
  providerBinding: string
  securityProfileBinding: string
  /** VM only. */
  storageClassBinding: string
  /** Container only: the application entry. */
  entryProtocol: 'http' | 'https'
  servicePort: number
  /** Container only: offer a browser terminal running this executable in /workspace. */
  terminal: boolean
  terminalExecutable: string
  network: 'deny_all' | 'allow_all'
  /** `YYYY-MM-DD`; Work created from this version is retained until then. */
  retainUntil: string
  confirmed: boolean
}

export function emptyManualDraft(now = Date.now()): ManualWorkTemplateDraft {
  return {
    imageId: '',
    name: '',
    description: '',
    templateFamilyId: '',
    reason: '',
    cpuMillicores: 2000,
    memoryGiB: 4,
    storageGiB: 20,
    providerBinding: '',
    securityProfileBinding: '',
    storageClassBinding: '',
    entryProtocol: 'http',
    servicePort: 8080,
    terminal: true,
    terminalExecutable: '/bin/bash',
    network: 'deny_all',
    retainUntil: new Date(now + 180 * DAY_MS).toISOString().slice(0, 10),
    confirmed: false,
  }
}

/** Copy the reviewed configuration of an earlier manual version, as the start of its next version. */
export function draftFromManualRelease(release: EnvironmentTemplateReleaseViewSchema, images: readonly TemplateImage[]): ManualWorkTemplateDraft | null {
  const manual = release.manualApproval
  if (!manual) return null
  const spec = manual.spec
  const image = images.find((entry) => entry.catalogId === manual.platformImage.catalogId)
  const container = spec.runtime.kind === 'container' ? spec.runtime : null
  const entry = container ? spec.entries.find((item) => item.servicePort === container.service_port) : undefined
  return {
    ...emptyManualDraft(),
    imageId: image?.catalogId ?? '',
    name: spec.name,
    description: manual.description,
    templateFamilyId: manual.templateFamilyId,
    cpuMillicores: spec.resources.cpuMillicores,
    memoryGiB: Math.max(1, Math.round(spec.resources.memoryBytes / GIB)),
    storageGiB: Math.max(1, Math.round(spec.resources.storageBytes / GIB)),
    providerBinding: spec.runtime.provider_binding,
    securityProfileBinding: spec.security.securityProfileBinding,
    storageClassBinding: spec.runtime.kind === 'virtual_machine' ? spec.runtime.storage_class_binding : '',
    entryProtocol: entry?.protocol === 'https' ? 'https' : 'http',
    servicePort: container?.service_port ?? 8080,
    terminal: Boolean(container?.terminal),
    terminalExecutable: container?.terminal?.executable ?? '/bin/bash',
    network: spec.network.mode === 'allow_all' ? 'allow_all' : 'deny_all',
  }
}

function positiveInteger(value: number): boolean {
  return Number.isSafeInteger(value) && value > 0
}

/** Why the draft cannot be published yet, in Chinese; null when it is complete. */
export function manualDraftProblem(draft: ManualWorkTemplateDraft, image: TemplateImage | null, now = Date.now()): string | null {
  if (!image) return '请选择一个平台镜像。'
  if (!draft.name.trim()) return '请填写模板名称。'
  const family = draft.templateFamilyId.trim()
  if (!family || family.length > MANUAL_TEMPLATE_LIMITS.templateFamilyId || !TEMPLATE_FAMILY_PATTERN.test(family)) {
    return '模板系列标识只能包含字母、数字、点、下划线和连字符，最长 128 个字符。'
  }
  if (draft.description.length > MANUAL_TEMPLATE_LIMITS.description) return '模板说明最长 4000 个字符。'
  if (!positiveInteger(draft.cpuMillicores) || !positiveInteger(draft.memoryGiB) || !positiveInteger(draft.storageGiB)) {
    return 'CPU、内存和存储都必须是大于 0 的整数。'
  }
  if (!draft.providerBinding.trim() || !draft.securityProfileBinding.trim()) return '请填写管理员提供的运行位置和安全配置。'
  if (image.kind === 'virtual_machine') {
    if (!draft.storageClassBinding.trim()) return '虚拟机模板需要填写管理员提供的磁盘存储类。'
    if (!positiveInteger(image.capacityBytes ?? 0)) return '这个虚拟机镜像缺少磁盘容量，不能用于发布。'
  } else {
    if (!Number.isInteger(draft.servicePort) || draft.servicePort < 1 || draft.servicePort > 65535) return '应用端口必须在 1 到 65535 之间。'
    if (draft.terminal && !/^\/[^\0]*$/.test(draft.terminalExecutable.trim())) return '终端程序必须是绝对路径，例如 /bin/bash。'
  }
  const retainUntil = Date.parse(`${draft.retainUntil}T00:00:00.000Z`)
  if (Number.isNaN(retainUntil) || retainUntil <= now) return '材料保留期限必须是今天之后的日期。'
  const reason = draft.reason.trim()
  if (!reason || reason.length > MANUAL_TEMPLATE_LIMITS.reason) return '请填写审核说明（最长 500 个字符）。'
  if (!draft.confirmed) return '请先确认已核对镜像、资源、入口和安全配置。'
  return null
}

/** The exact EnvironmentSpec the reviewer approves. `policyId` identifies this publication's retention decision. */
export function manualTemplateSpec(draft: ManualWorkTemplateDraft, image: TemplateImage, policyId: string): EnvironmentSpec {
  const resources = {
    cpuMillicores: draft.cpuMillicores,
    memoryBytes: draft.memoryGiB * GIB,
    storageBytes: draft.storageGiB * GIB,
  }
  const security = {
    runAsUser: null,
    allowPrivilegeEscalation: false as const,
    readOnlyRootFilesystem: false,
    publicExposurePolicy: 'deny' as const,
    securityProfileBinding: draft.securityProfileBinding.trim(),
  }
  const retention = {
    policyId,
    policyRevision: 1,
    class: 'run_evidence' as const,
    disposition: 'delete' as const,
    retainUntil: `${draft.retainUntil}T00:00:00.000Z`,
  }
  const base = { apiVersion: 'environment.labweaver.io/v1' as const, kind: 'EnvironmentSpec' as const, class: 'work' as const, name: draft.name.trim(), resources, retention }
  if (image.kind === 'virtual_machine') {
    return {
      ...base,
      network: { mode: 'deny_all' },
      entries: [{ name: 'ssh', protocol: 'ssh', servicePort: 22 }],
      security,
      runtime: {
        kind: 'virtual_machine',
        provider_binding: draft.providerBinding.trim(),
        storage_class_binding: draft.storageClassBinding.trim(),
        ssh_port: 22,
        base_disk: { binding: image.binding, capacityBytes: image.capacityBytes ?? 0, sourceRegistryDigest: image.resolvedDigest },
      },
    }
  }
  return {
    ...base,
    network: { mode: draft.network },
    entries: [{ name: 'app', protocol: draft.entryProtocol, servicePort: draft.servicePort }],
    security,
    runtime: {
      kind: 'container',
      provider_binding: draft.providerBinding.trim(),
      service_port: draft.servicePort,
      ...(draft.terminal ? { terminal: { executable: draft.terminalExecutable.trim(), args: [], workingDirectory: '/workspace' } } : {}),
    },
  }
}

/** Request body for a draft, with the retention identity left out so equal content fingerprints equally. */
function publicationBody(projectId: string, courseId: string | null, draft: ManualWorkTemplateDraft, image: TemplateImage, policyId: string): CreateEnvironmentTemplateReleaseRequestSchema {
  return {
    projectId,
    ...(courseId ? { courseId } : {}),
    runtimeKind: image.kind,
    manual: {
      platformImageId: image.catalogId,
      platformImageGeneration: image.repinGeneration,
      templateFamilyId: draft.templateFamilyId.trim(),
      description: draft.description.trim(),
      reason: draft.reason.trim(),
      spec: manualTemplateSpec(draft, image, policyId),
      confirmed: true,
    },
  }
}

/** What a pending manual publication keeps besides its key: the reviewed draft and its retention identity. */
interface PendingPublication {
  policyId: string
  draft: ManualWorkTemplateDraft
}

/**
 * Control answers with these codes only after its idempotency ledger reserved
 * a key it had never seen and before the publication transaction commits (the
 * reservation rolls back with it), so a replay of an accepted publication can
 * never produce them: the command is proven not to have been accepted. Every
 * other answer, including 401, 403 (checked before the ledger), a retryable
 * 409 such as an operation in progress, 5xx, a transport failure or an
 * unknown code, leaves the outcome unknown.
 */
const NOT_ACCEPTED_CODES: readonly string[] = ['LW_CONTRACT_DOCUMENT_INVALID', 'LW_MATERIAL_RETENTION_EXPIRED', 'LW_RELEASE_ARTIFACT_MISMATCH']

export type PublicationOutcome =
  | { kind: 'accepted' }
  /** `unconfirmed`: the publication may have been accepted; retrying the same content replays it. */
  | { kind: 'failed'; diagnostic: DiagnosticViewModel; unconfirmed: boolean }

/**
 * Manual Work template publication: the project's pinned platform images and
 * one reviewed, confirmed publication command.
 *
 * A command without a definite outcome is kept in session storage for the
 * signed-in actor and project, keyed by its canonical content. A retry, a page
 * refresh or reopening the form with the same content replays it with the same
 * Idempotency-Key and retention identity, so a lost response never publishes a
 * second version; changed content is a new command. The entry is removed only
 * when that exact command is accepted or proven not accepted.
 */
export function useManualWorkTemplatePublication(
  projectId: Ref<string | null | undefined>,
  courseId: Ref<string | null | undefined>,
  actorId: Ref<string | null | undefined>,
) {
  const images = ref<AsyncState<TemplateImage[]>>({ kind: 'idle' })
  const publishing = ref(false)
  const outcome = ref<PublicationOutcome | null>(null)
  /** The reviewed draft of a publication whose outcome is still unknown, restored after a refresh. */
  const pendingDraft = ref<ManualWorkTemplateDraft | null>(null)
  let generation = 0
  /** Changes with the project or actor; an answer for an older context only updates storage. */
  let context = 0

  const scope = (id: string) => `manual-work-template:${id}`

  async function loadImages() {
    const id = projectId.value
    const current = ++generation
    if (!id) {
      images.value = { kind: 'idle' }
      return
    }
    images.value = { kind: 'loading', message: '加载平台镜像…' }
    const result = await listProjectTemplateImages({ path: { projectId: id } }).catch((error: unknown) => ({ data: undefined, error }))
    if (current !== generation) return
    if (!result.data) {
      images.value = { kind: 'error', diagnostic: diagnosticFromError(result.error, 'TEMPLATE_IMAGES_LOAD_FAILED', '加载平台镜像失败。') }
      return
    }
    const active = result.data.entries.filter((entry) => entry.status === 'active')
    images.value = active.length ? { kind: 'success', data: active } : { kind: 'empty' }
  }

  /** Resolves to true only when this context's publication was accepted. */
  async function publish(draft: ManualWorkTemplateDraft): Promise<boolean> {
    const id = projectId.value
    const actor = actorId.value
    const image = images.value.kind === 'success' ? images.value.data.find((entry) => entry.catalogId === draft.imageId) ?? null : null
    if (!id || !actor || publishing.value || !image || manualDraftProblem(draft, image)) return false
    const course = courseId.value ?? null
    const fingerprint = JSON.stringify(publicationBody(id, course, draft, image, ''))
    const stored = readPendingCommand<PendingPublication>(actor, scope(id))
    const command = stored?.fingerprint === fingerprint && typeof stored.data?.policyId === 'string'
      ? stored
      : { fingerprint, idempotencyKey: idempotencyKey(), data: { policyId: newUuidV7(), draft: { ...draft } } }
    // Recorded before sending: a response lost after this point is replayed, never re-minted.
    writePendingCommand(actor, scope(id), command)
    const requestContext = context
    const current = () => requestContext === context
    publishing.value = true
    outcome.value = null
    let next: PublicationOutcome
    try {
      const result = await createEnvironmentTemplateRelease({
        path: { projectId: id },
        headers: { 'Idempotency-Key': command.idempotencyKey },
        body: publicationBody(id, course, draft, image, command.data.policyId),
      })
      if (result.error || !result.data) {
        const diagnostic = diagnosticFromError(result.error, 'MANUAL_TEMPLATE_PUBLISH_FAILED', '发布 Work 模板失败。')
        const notAccepted = NOT_ACCEPTED_CODES.includes(diagnostic.code)
        if (notAccepted) clearPendingCommand(actor, scope(id), command.idempotencyKey)
        next = { kind: 'failed', diagnostic, unconfirmed: !notAccepted }
      } else {
        clearPendingCommand(actor, scope(id), command.idempotencyKey)
        next = { kind: 'accepted' }
      }
    } catch (error) {
      next = { kind: 'failed', diagnostic: diagnosticFromError(error, 'MANUAL_TEMPLATE_PUBLISH_FAILED', '发布结果尚未确认。'), unconfirmed: true }
    }
    if (!current()) return false
    publishing.value = false
    outcome.value = next
    if (next.kind === 'accepted') pendingDraft.value = null
    return next.kind === 'accepted'
  }

  watch([projectId, actorId], ([id, actor]) => {
    context += 1
    publishing.value = false
    outcome.value = null
    pendingDraft.value = id && actor ? readPendingCommand<PendingPublication>(actor, scope(id))?.data?.draft ?? null : null
  }, { immediate: true })
  watch(projectId, () => { void loadImages() }, { immediate: true })

  return reactive({ images, publishing, outcome, pendingDraft, loadImages, publish })
}
