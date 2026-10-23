import { computed, ref, type Ref } from 'vue'
import { freezeSubmission, getFrozenSubmission } from '@/generated/contracts'
import type {
  EnvironmentInstanceSchema,
  EnvironmentOperationSnapshotSchema,
  EnvironmentTemplateReleaseViewSchema,
  OperationAccepted,
  SubmissionManifest,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import type { EnvironmentInstanceWithFreeze } from '@/types/access'
import { useCurrentActorId } from '@/composables/useSubmissionDirectory'
import { rememberSubmission } from '@/utils/trackedSubmissions'
import { idempotencyKey, ifMatch } from '@/utils/format'

type InstanceState = { kind: string; data?: EnvironmentInstanceSchema }
type ReleasesState = AsyncState<readonly EnvironmentTemplateReleaseViewSchema[]> | AsyncState<EnvironmentTemplateReleaseViewSchema[]>

export interface EnvironmentSubmissionDeps {
  env: { instance: InstanceState; load: () => Promise<unknown> }
  operations: { operations: AsyncState<EnvironmentOperationSnapshotSchema[]>; load: () => Promise<unknown> }
  releases: { releases: ReleasesState }
  selectedEnvironmentId: Ref<string | undefined>
  isWorkConnection: Ref<boolean>
}

interface FreezeAcceptance {
  environmentId: string
  projectId: string
  operationId: string
  statusUrl: string
  state: EnvironmentOperationSnapshotSchema['state']
}

/** Freeze-and-submit for one experiment environment. */
export function useEnvironmentSubmission(deps: EnvironmentSubmissionDeps) {
  const { env, operations, releases, selectedEnvironmentId, isWorkConnection } = deps
  const actorId = useCurrentActorId()

  const freezeState = ref<AsyncState<OperationAccepted>>({ kind: 'idle' })
  const freezeDiagnostic = ref<DiagnosticViewModel | null>(null)
  const lastFreezeEnvironmentId = ref<string | null>(null)
  const freezeAccepted = ref<FreezeAcceptance | null>(null)
  const freezeConfirmVisible = ref(false)
  /** A freeze can drop the browser terminal; the console tab offers a reconnect. */
  const consoleReconnectRequired = ref(false)
  // One Idempotency-Key per freeze intent, kept across retries until a terminal
  // outcome, so a timeout + retry cannot mint a duplicate frozen submission.
  const freezeIntentKey = ref<string | null>(null)
  // Freeze evidence is deliberately local: the environment instance is replaced
  // wholesale by polling, and the public contract does not embed freeze evidence
  // on the instance, so injecting it there would silently disappear.
  const frozenSubmission = ref<{
    environmentId: string
    submissionId: string
    object: NonNullable<EnvironmentInstanceSchema['cleanupEvidence']>
    contentSha256: string
    frozenAt: string
  } | null>(null)
  /** Bumped whenever a submission is remembered for progress tracking. */
  const trackedSubmissionsVersion = ref(0)

  const freezeOperation = computed<EnvironmentOperationSnapshotSchema | null>(() => {
    if (operations.operations.kind !== 'success') return null
    const environmentId = selectedEnvironmentId.value
    if (!environmentId) return null
    const freezeOperations = operations.operations.data
      .filter((operation) => operation.environmentId === environmentId && operation.kind === 'freeze')
      .sort((left, right) => right.acceptedAt.localeCompare(left.acceptedAt))
    const trackedOperationId = freezeAccepted.value?.operationId
    return (trackedOperationId
      ? freezeOperations.find((operation) => operation.operationId === trackedOperationId)
      : null) ?? freezeOperations[0] ?? null
  })

  const freezeStatus = computed<EnvironmentOperationSnapshotSchema['state'] | null>(() =>
    freezeOperation.value?.state
      ?? (freezeAccepted.value && freezeAccepted.value.environmentId === selectedEnvironmentId.value ? freezeAccepted.value.state : null)
      ?? (frozenSubmission.value?.environmentId === selectedEnvironmentId.value ? 'succeeded' : null),
  )

  const freezeSubmissionPending = computed(() =>
    freezeState.value.kind === 'loading'
    || freezeStatus.value === 'accepted'
    || freezeStatus.value === 'running'
    || freezeStatus.value === 'cancelling',
  )

  function releaseForEnvironment(data: EnvironmentInstanceSchema): EnvironmentTemplateReleaseViewSchema | null {
    const state = releases.releases
    const rows = state.kind === 'success' ? state.data : []
    return rows.find((release) => release.id === data.releaseId && release.version === data.releaseVersion) ?? null
  }

  function selectedSubmissionManifestFor(data: EnvironmentInstanceSchema): SubmissionManifest | null {
    if (data.class === 'work' || isWorkConnection.value) return null
    const release = releaseForEnvironment(data)
    if (!release || release.withdrawal) return null
    return release.submissionManifest ?? null
  }

  function freezeBlockReason(data: EnvironmentInstanceSchema): string | null {
    if (data.class === 'work' || isWorkConnection.value) return '此 Work 环境未配置评测，无法提交评测。'
    if (data.observedState !== 'ready') return '环境必须处于就绪状态才能提交评测。'
    const releaseState = releases.releases
    if (releaseState.kind === 'idle' || releaseState.kind === 'loading') return '正在确认当前环境的提交内容。'
    if (releaseState.kind === 'error' || releaseState.kind === 'unauthorized' || releaseState.kind === 'blocked') {
      return `暂时无法确认当前环境的提交内容：${releaseState.diagnostic.message}`
    }
    if (releaseState.kind === 'empty') return '当前项目没有可用的已发布版本，无法提交评测。'
    const release = releaseForEnvironment(data)
    if (!release) return '未找到当前环境对应的已发布版本，无法提交评测。'
    if (release.withdrawal) return '当前环境所用版本已撤回，无法提交评测。'
    if (!release.submissionManifest) return '此版本未配置提交评测，无法提交评测。'
    return null
  }

  function freezeManifestText(data: EnvironmentInstanceSchema): string {
    const manifest = selectedSubmissionManifestFor(data)
    return manifest ? JSON.stringify(manifest, null, 2) : ''
  }

  function canFreeze(data: EnvironmentInstanceSchema): boolean {
    return data.observedState === 'ready'
      && selectedSubmissionManifestFor(data) !== null
      && freezeState.value.kind !== 'loading'
  }

  function fail(code: string, message: string, retryable: boolean) {
    freezeState.value = { kind: 'error', diagnostic: makeDiagnostic(code, message, retryable) }
    freezeDiagnostic.value = freezeState.value.diagnostic
  }

  async function freeze(data: EnvironmentInstanceSchema) {
    freezeDiagnostic.value = null
    const previousFreezeEnvironmentId = lastFreezeEnvironmentId.value
    const manifest = selectedSubmissionManifestFor(data)
    if (!manifest) {
      fail('SUBMISSION_MANIFEST_UNAVAILABLE', freezeBlockReason(data) ?? '当前环境尚未配置提交评测，无法提交评测。', false)
      freezeConfirmVisible.value = false
      return
    }
    lastFreezeEnvironmentId.value = data.id
    if (!freezeIntentKey.value || previousFreezeEnvironmentId !== data.id) {
      if (previousFreezeEnvironmentId !== data.id) {
        freezeAccepted.value = null
        consoleReconnectRequired.value = false
      }
      freezeIntentKey.value = idempotencyKey()
    }
    const intentKey = freezeIntentKey.value
    freezeState.value = { kind: 'loading', message: '冻结提交中…' }
    const result = await freezeSubmission({
      path: { environmentId: data.id },
      headers: { 'Idempotency-Key': intentKey, 'If-Match': ifMatch(data.revision) },
      body: {
        ...(data.courseId ? { courseId: data.courseId } : {}),
        manifest,
      },
    })
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      freezeState.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(problem?.diagnosticCode ?? 'FREEZE_FAILED', problem?.detail ?? '冻结提交失败', problem?.retryable ?? true, problem),
      }
      freezeDiagnostic.value = freezeState.value.diagnostic
      if (!(problem?.retryable ?? true)) freezeIntentKey.value = null
      return
    }
    freezeState.value = { kind: 'success', data: result.data }
    const statusMatch = result.data.statusUrl.match(/^\/api\/v1\/projects\/([^/?#]+)\/frozen-submissions\/([0-9a-f-]{36})$/)
    const submissionId = statusMatch?.[2]
    if (!statusMatch || statusMatch[1] !== data.projectId || !submissionId) {
      fail('FREEZE_STATUS_IDENTITY_INVALID', '冻结提交返回了无法验证的状态地址', false)
      freezeIntentKey.value = null
      return
    }
    freezeAccepted.value = {
      environmentId: data.id,
      projectId: data.projectId,
      operationId: result.data.operationId,
      statusUrl: result.data.statusUrl,
      state: 'accepted',
    }
    consoleReconnectRequired.value = true
    await operations.load()
    let frozenObject: NonNullable<EnvironmentInstanceWithFreeze['freezeEvidence']> | undefined
    let frozenContentSha256: string | null = null
    let frozenAt: string | null = null
    for (let attempt = 0; attempt < 30; attempt += 1) {
      if (attempt > 0 && attempt % 3 === 0) await operations.load()
      const frozen = await getFrozenSubmission({ path: { projectId: data.projectId, submissionId } })
      if (frozen.data) {
        frozenObject = frozen.data.object
        frozenContentSha256 = frozen.data.contentSha256
        frozenAt = frozen.data.frozenAt
        break
      }
      const problem = extractProblemDetails(frozen.error)
      if (problem && problem.diagnosticCode !== 'LW_COLLECT_SUBMISSION_NOT_FOUND' && !problem.retryable) {
        fail(problem.diagnosticCode ?? 'FREEZE_READBACK_FAILED', problem.detail ?? '冻结提交读取失败', false)
        freezeIntentKey.value = null
        return
      }
      await new Promise<void>((resolve) => window.setTimeout(resolve, 1000))
    }
    if (!frozenObject) {
      fail('FREEZE_READBACK_TIMEOUT', '冻结提交读取超时', true)
      return
    }
    frozenSubmission.value = {
      environmentId: data.id,
      submissionId,
      object: frozenObject,
      contentSha256: frozenContentSha256 ?? '',
      frozenAt: frozenAt ?? '',
    }
    rememberSubmission(actorId.value, data.projectId, submissionId)
    trackedSubmissionsVersion.value += 1
    if (freezeAccepted.value?.operationId === result.data.operationId) freezeAccepted.value.state = 'succeeded'
    freezeIntentKey.value = null
    await env.load()
    await operations.load()
  }

  async function confirmFreeze(data: EnvironmentInstanceSchema) {
    freezeConfirmVisible.value = false
    await freeze(data)
  }

  function freezeEvidenceFor(data: EnvironmentInstanceSchema) {
    return frozenSubmission.value && frozenSubmission.value.environmentId === data.id ? frozenSubmission.value : null
  }

  async function retryFreeze() {
    const id = lastFreezeEnvironmentId.value ?? freezeOperation.value?.environmentId ?? selectedEnvironmentId.value
    if (!id) return
    const instance = env.instance.kind === 'success' ? env.instance.data : undefined
    if (instance && instance.id === id) await freeze(instance)
  }

  return {
    freezeState,
    freezeDiagnostic,
    freezeConfirmVisible,
    consoleReconnectRequired,
    freezeOperation,
    freezeStatus,
    freezeSubmissionPending,
    selectedSubmissionManifestFor,
    freezeBlockReason,
    freezeManifestText,
    canFreeze,
    confirmFreeze,
    freezeEvidenceFor,
    retryFreeze,
    trackedSubmissionsVersion,
  }
}
