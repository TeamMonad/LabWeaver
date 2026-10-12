import { computed, reactive, ref } from 'vue'
import {
  applyPlatformConfiguration,
  getPlatformConfiguration,
  getPlatformConfigurationApply,
  getPlatformConfigurationVersion,
  updatePlatformConfiguration,
} from '@/generated/contracts'
import type {
  PlatformConfigurationApply,
  PlatformConfigurationDesired,
  PlatformConfigurationStatusSchema,
  PlatformConfigurationVersionSchema,
  ProblemDetails,
} from '@/generated/contracts'
import { extractProblemDetails } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'
import { canonicalJson, cloneConfiguration, parseRequestYaml } from '@/utils/platformConfiguration'

/** A failure shown to the administrator: Chinese reason and recovery first, identifiers after. */
export interface PlatformConfigurationFailure {
  message: string
  recovery: string
  code: string
  requestId?: string
  traceId?: string
  status?: number
}

export interface PlatformConfigurationConflict {
  /** The server facts read after the 409/412; the draft is kept untouched. */
  latest: PlatformConfigurationStatusSchema
  etag: string
  /** Configuration the rejected draft was edited from. */
  base: PlatformConfigurationDesired | null
}

type ResponseLike = { response?: { headers?: Record<string, unknown> | { get?: (name: string) => unknown } } }

function readEtag(result: ResponseLike, revision: number): string {
  const headers = result.response?.headers as Record<string, unknown> | undefined
  const raw = headers?.etag ?? headers?.ETag
  return typeof raw === 'string' && raw ? raw : ifMatch(revision)
}

function failureFrom(error: unknown, fallbackMessage: string, fallbackRecovery: string): PlatformConfigurationFailure {
  const problem: ProblemDetails | undefined = extractProblemDetails(error)
  const status = problem?.status
  const recovery = status === 403
    ? '只有平台管理员可以读取或修改平台配置；请使用管理员账号登录。'
    : status === 401
      ? '会话已失效，请重新登录后再试。'
      : status === 422 || status === 400
        ? '检查标出的字段后重新提交；草稿已保留。'
        : problem?.retryable
          ? '稍后重试；草稿已保留。'
          : fallbackRecovery
  return {
    message: problem?.detail ?? fallbackMessage,
    recovery,
    code: problem?.diagnosticCode ?? 'LW_PLATFORM_CONFIGURATION_REQUEST_FAILED',
    requestId: problem?.requestId,
    traceId: problem?.traceId,
    status,
  }
}

function isRevisionConflict(error: unknown): boolean {
  const status = extractProblemDetails(error)?.status
  return status === 409 || status === 412
}

/**
 * Administrator platform configuration over the Control SDK.
 *
 * The server is the only writer of revisions, desired versions and apply
 * facts. This composable keeps one editable draft copied from the server,
 * submits it with the observed revision/ETag, keeps the draft on any failure,
 * and never advances local state from an older replayed response.
 */
export function usePlatformConfiguration() {
  const status = ref<PlatformConfigurationStatusSchema | null>(null)
  const etag = ref('')
  const loadState = ref<'idle' | 'loading' | 'ready' | 'error'>('idle')
  const loadFailure = ref<PlatformConfigurationFailure | null>(null)

  const draft = ref<PlatformConfigurationDesired | null>(null)
  /** Server configuration the draft was copied from (desired, or defaults when unconfigured). */
  const draftBase = ref<PlatformConfigurationDesired | null>(null)
  /** Revision the next update must name in expectedRevision / If-Match. */
  const baseRevision = ref(0)
  const baseEtag = ref('')

  const saving = ref(false)
  const applying = ref(false)
  const actionFailure = ref<PlatformConfigurationFailure | null>(null)
  const conflict = ref<PlatformConfigurationConflict | null>(null)
  const notice = ref<string | null>(null)

  const applyHistory = ref<PlatformConfigurationApply[]>([])
  const historyLoading = ref(false)
  const historyFailure = ref<PlatformConfigurationFailure | null>(null)

  // Idempotency keys stay stable while the same request is retried, and change with the request.
  let saveKey: { fingerprint: string; key: string } | null = null
  let applyKey: { fingerprint: string; key: string } | null = null

  function stableKey(slot: 'save' | 'apply', fingerprint: string): string {
    const current = slot === 'save' ? saveKey : applyKey
    if (current && current.fingerprint === fingerprint) return current.key
    const next = { fingerprint, key: idempotencyKey() }
    if (slot === 'save') saveKey = next
    else applyKey = next
    return next.key
  }

  const serverConfiguration = computed<PlatformConfigurationDesired | null>(() =>
    status.value?.desired?.configuration ?? status.value?.defaults ?? null,
  )

  const draftDirty = computed(() =>
    draft.value !== null && draftBase.value !== null && canonicalJson(draft.value) !== canonicalJson(draftBase.value),
  )

  function resetDraftFromServer(): void {
    const source = serverConfiguration.value
    draft.value = source ? cloneConfiguration(source) : null
    draftBase.value = source ? cloneConfiguration(source) : null
    baseRevision.value = status.value?.revision ?? 0
    baseEtag.value = etag.value
  }

  /** Accept server facts unless they are older than facts already shown (idempotent replay). */
  function acceptStatus(next: PlatformConfigurationStatusSchema, nextEtag: string): boolean {
    if (status.value && next.revision < status.value.revision) return false
    status.value = next
    etag.value = nextEtag
    if (next.latestApply && !applyHistory.value.some((item) => item.id === next.latestApply?.id)) {
      applyHistory.value = [next.latestApply]
    } else if (next.latestApply) {
      applyHistory.value = applyHistory.value.map((item) => (item.id === next.latestApply?.id ? next.latestApply : item))
    }
    return true
  }

  async function fetchStatus(): Promise<{ data: PlatformConfigurationStatusSchema; etag: string } | { failure: PlatformConfigurationFailure }> {
    const result = await getPlatformConfiguration()
    if (result.error || !result.data) {
      return { failure: failureFrom(result.error, '读取平台配置失败。', '稍后刷新重试。') }
    }
    return { data: result.data, etag: readEtag(result as ResponseLike, result.data.revision) }
  }

  async function load(options: { keepDraft?: boolean } = {}): Promise<void> {
    loadState.value = 'loading'
    loadFailure.value = null
    const outcome = await fetchStatus()
    if ('failure' in outcome) {
      loadFailure.value = outcome.failure
      loadState.value = status.value ? 'ready' : 'error'
      return
    }
    status.value = null
    acceptStatus(outcome.data, outcome.etag)
    applyHistory.value = outcome.data.latestApply ? [outcome.data.latestApply] : []
    if (!options.keepDraft || draft.value === null) resetDraftFromServer()
    loadState.value = 'ready'
  }

  async function handleConflict(): Promise<void> {
    const outcome = await fetchStatus()
    if ('failure' in outcome) {
      actionFailure.value = {
        ...outcome.failure,
        message: '配置已被其他管理员更新，但重新读取最新配置失败。',
        recovery: '草稿已保留。稍后点击刷新，再比较并重新提交。',
      }
      return
    }
    conflict.value = { latest: outcome.data, etag: outcome.etag, base: draftBase.value ? cloneConfiguration(draftBase.value) : null }
  }

  async function submitUpdate(mode: 'form' | 'yaml', yamlText?: string): Promise<boolean> {
    if (!draft.value || saving.value) return false
    actionFailure.value = null
    notice.value = null
    let body = { expectedRevision: baseRevision.value, configuration: draft.value }
    let yamlBody: string | null = null
    if (mode === 'yaml') {
      const parsed = parseRequestYaml(yamlText ?? '')
      if (!parsed.ok) {
        actionFailure.value = { message: parsed.message, recovery: '修正 YAML 后重新提交；内容未发送。', code: 'LW_PLATFORM_CONFIGURATION_YAML_INVALID' }
        return false
      }
      if (parsed.request.expectedRevision !== baseRevision.value) {
        actionFailure.value = {
          message: `YAML 中的 expectedRevision（${parsed.request.expectedRevision}）与当前编辑基线 revision ${baseRevision.value} 不一致。`,
          recovery: '将 expectedRevision 改为当前基线，或刷新后在最新配置上重新编辑；内容未发送。',
          code: 'LW_PLATFORM_CONFIGURATION_REVISION_MISMATCH',
        }
        return false
      }
      body = parsed.request
      yamlBody = yamlText ?? ''
    }
    const fingerprint = `${mode}:${yamlBody ?? canonicalJson(body)}`
    saving.value = true
    try {
      const result = await updatePlatformConfiguration({
        headers: {
          'Idempotency-Key': stableKey('save', fingerprint),
          'If-Match': baseEtag.value || ifMatch(baseRevision.value),
          ...(yamlBody !== null ? { 'Content-Type': 'application/yaml' } : {}),
        } as Parameters<typeof updatePlatformConfiguration>[0]['headers'],
        body,
        ...(yamlBody !== null ? { bodySerializer: () => yamlBody } : {}),
      })
      if (result.error || !result.data) {
        if (isRevisionConflict(result.error)) {
          await handleConflict()
          return false
        }
        actionFailure.value = failureFrom(result.error, '保存平台配置失败。', '草稿已保留，检查后重新提交。')
        return false
      }
      saveKey = null
      conflict.value = null
      const accepted = acceptStatus(result.data, readEtag(result as ResponseLike, result.data.revision))
      if (!accepted) {
        notice.value = '收到的是较早请求的回放结果，已保留更新的服务端事实。'
        await load({ keepDraft: true })
        return true
      }
      resetDraftFromServer()
      notice.value = `已保存为 desired revision ${result.data.desired?.revision ?? result.data.revision}。保存不等于部署；需要提交应用请求并由获准执行者执行。`
      return true
    } finally {
      saving.value = false
    }
  }

  /** Keep the draft and edit on top of the newer server revision after a conflict. */
  function rebaseDraftOnLatest(): void {
    if (!conflict.value) return
    acceptStatus(conflict.value.latest, conflict.value.etag)
    const source = serverConfiguration.value
    draftBase.value = source ? cloneConfiguration(source) : null
    baseRevision.value = conflict.value.latest.revision
    baseEtag.value = conflict.value.etag
    conflict.value = null
    saveKey = null
  }

  /** Drop the draft and continue from the newer server configuration. */
  function discardDraftForLatest(): void {
    if (!conflict.value) return
    acceptStatus(conflict.value.latest, conflict.value.etag)
    conflict.value = null
    saveKey = null
    resetDraftFromServer()
  }

  async function requestApply(encryptedBundleSha256: string): Promise<boolean> {
    const current = status.value
    if (!current?.desired || applying.value) return false
    actionFailure.value = null
    notice.value = null
    const body = {
      expectedRevision: current.revision,
      desiredRevision: current.desired.revision,
      encryptedBundleSha256,
    }
    applying.value = true
    try {
      const result = await applyPlatformConfiguration({
        headers: {
          'Idempotency-Key': stableKey('apply', canonicalJson(body)),
          'If-Match': etag.value || ifMatch(current.revision),
        } as Parameters<typeof applyPlatformConfiguration>[0]['headers'],
        body,
      })
      if (result.error || !result.data) {
        if (isRevisionConflict(result.error)) {
          actionFailure.value = {
            ...failureFrom(result.error, '配置 revision 已变化，应用请求未记录。', ''),
            recovery: '刷新查看最新 desired，确认要应用的版本后重新提交。',
          }
          return false
        }
        actionFailure.value = failureFrom(result.error, '记录应用请求失败。', '核对 Bundle 摘要后重试。')
        return false
      }
      applyKey = null
      acceptStatus(result.data, readEtag(result as ResponseLike, result.data.revision))
      notice.value = '应用请求已记录（202）。这只表示请求已登记，部署需由获准执行者执行，并以其报告的阶段为准。'
      return true
    } finally {
      applying.value = false
    }
  }

  async function loadPreviousApply(): Promise<void> {
    const last = applyHistory.value[applyHistory.value.length - 1]
    if (!last?.previousApplyId || historyLoading.value) return
    historyLoading.value = true
    historyFailure.value = null
    try {
      const result = await getPlatformConfigurationApply({ path: { applyId: last.previousApplyId } })
      if (result.error || !result.data) {
        historyFailure.value = failureFrom(result.error, '读取上一次应用尝试失败。', '稍后重试。')
        return
      }
      applyHistory.value = [...applyHistory.value, result.data]
    } finally {
      historyLoading.value = false
    }
  }

  async function refreshApply(applyId: string): Promise<void> {
    const result = await getPlatformConfigurationApply({ path: { applyId } })
    if (result.error || !result.data) {
      historyFailure.value = failureFrom(result.error, '刷新应用尝试失败。', '稍后重试。')
      return
    }
    const fresh = result.data
    applyHistory.value = applyHistory.value.map((item) => (item.id === applyId ? fresh : item))
  }

  async function readVersion(revision: number): Promise<{ data: PlatformConfigurationVersionSchema } | { failure: PlatformConfigurationFailure }> {
    const result = await getPlatformConfigurationVersion({ path: { revision } })
    if (result.error || !result.data) {
      return { failure: failureFrom(result.error, `读取 revision ${revision} 失败。`, '稍后重试。') }
    }
    return { data: result.data }
  }

  return reactive({
    status,
    etag,
    loadState,
    loadFailure,
    draft,
    draftBase,
    baseRevision,
    draftDirty,
    saving,
    applying,
    actionFailure,
    conflict,
    notice,
    applyHistory,
    historyLoading,
    historyFailure,
    load,
    submitUpdate,
    rebaseDraftOnLatest,
    discardDraftForLatest,
    resetDraftFromServer,
    requestApply,
    loadPreviousApply,
    refreshApply,
    readVersion,
  })
}
