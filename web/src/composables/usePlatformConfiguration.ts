import { computed, reactive, ref } from 'vue'
import {
  applyPlatformConfiguration,
  getPlatformConfiguration,
  getPlatformConfigurationApply,
  getPlatformConfigurationVersion,
  registerPlatformConfigurationBundle,
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

/** Maximum encrypted Bundle upload accepted by Control (70 MiB). */
export const PLATFORM_CONFIGURATION_BUNDLE_MAX_BYTES = 73_400_320

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
  const code = problem?.diagnosticCode ?? 'LW_PLATFORM_CONFIGURATION_REQUEST_FAILED'
  let recovery = fallbackRecovery
  if (status === 403) {
    recovery = '只有平台管理员可以读取或修改平台配置；请使用管理员账号登录。'
  } else if (status === 401) {
    recovery = '会话已失效，请重新登录后再试。'
  } else if (code === 'LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED') {
    recovery = '请先选择实际 .age 密文文件完成 Bundle 登记，再用登记成功后的服务端事实提交应用请求。'
  } else if (status === 422 || status === 400) {
    recovery = '检查标出的字段后重新提交；草稿与所选文件已保留。'
  } else if (status === 413) {
    recovery = `上传文件不能超过 ${PLATFORM_CONFIGURATION_BUNDLE_MAX_BYTES} 字节（约 70 MiB）；所选文件已保留。`
  } else if (problem?.retryable) {
    recovery = '稍后重试；草稿与所选文件已保留。可用相同文件与原幂等键重试，不要重新 seal。'
  }
  return {
    message: problem?.detail ?? fallbackMessage,
    recovery,
    code,
    requestId: problem?.requestId,
    traceId: problem?.traceId,
    status,
  }
}

function isRevisionConflict(error: unknown): boolean {
  const status = extractProblemDetails(error)?.status
  return status === 409 || status === 412
}

function fileFingerprint(file: File, desiredRevision: number, configurationSha256: string): string {
  return `register:${desiredRevision}:${configurationSha256}:${file.name}:${file.size}:${file.lastModified}`
}

/**
 * Administrator platform configuration over the Control SDK.
 *
 * The server is the only writer of revisions, desired versions, registration
 * digests and apply facts. This composable keeps one editable draft copied from
 * the server, submits it with the observed revision/ETag, keeps the draft and
 * selected Bundle file on any failure, and never advances local state from an
 * older replayed response.
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
  const registering = ref(false)
  const applying = ref(false)
  const actionFailure = ref<PlatformConfigurationFailure | null>(null)
  const conflict = ref<PlatformConfigurationConflict | null>(null)
  const notice = ref<string | null>(null)

  /** Selected ciphertext file for register; kept across 409/412 and network retries. */
  const selectedBundleFile = ref<File | null>(null)

  const applyHistory = ref<PlatformConfigurationApply[]>([])
  const historyLoading = ref(false)
  const historyFailure = ref<PlatformConfigurationFailure | null>(null)

  // Idempotency keys stay stable while the same request is retried, and change with the request.
  let saveKey: { fingerprint: string; key: string } | null = null
  let registerKey: { fingerprint: string; key: string } | null = null
  let applyKey: { fingerprint: string; key: string } | null = null

  function stableKey(slot: 'save' | 'register' | 'apply', fingerprint: string): string {
    const current = slot === 'save' ? saveKey : slot === 'register' ? registerKey : applyKey
    if (current && current.fingerprint === fingerprint) return current.key
    const next = { fingerprint, key: idempotencyKey() }
    if (slot === 'save') saveKey = next
    else if (slot === 'register') registerKey = next
    else applyKey = next
    return next.key
  }

  const serverConfiguration = computed<PlatformConfigurationDesired | null>(() =>
    status.value?.desired?.configuration ?? status.value?.defaults ?? null,
  )

  const draftDirty = computed(() =>
    draft.value !== null && draftBase.value !== null && canonicalJson(draft.value) !== canonicalJson(draftBase.value),
  )

  const registeredBundle = computed(() => status.value?.registeredBundle ?? null)

  const canApply = computed(() => {
    const current = status.value
    const registered = current?.registeredBundle
    return Boolean(
      current?.desired
      && registered
      && registered.desiredRevision === current.desired.revision
      && !applying.value
      && !registering.value
      && conflict.value === null,
    )
  })

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
        recovery: '草稿与所选文件已保留。稍后点击刷新，再比较并重新提交。',
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
      // New desired clears the registration pointer server-side; drop stale register idempotency.
      registerKey = null
      notice.value = `已保存为 desired revision ${result.data.desired?.revision ?? result.data.revision}。保存会清空当前 Bundle 登记，需要重新选择 .age 文件登记后再提交应用；保存不等于部署。`
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
    registerKey = null
  }

  /** Drop the draft and continue from the newer server configuration. */
  function discardDraftForLatest(): void {
    if (!conflict.value) return
    acceptStatus(conflict.value.latest, conflict.value.etag)
    conflict.value = null
    saveKey = null
    registerKey = null
    resetDraftFromServer()
  }

  function selectBundleFile(file: File | null): void {
    selectedBundleFile.value = file
    registerKey = null
    actionFailure.value = null
  }

  /**
   * Upload the selected .age ciphertext. Digests come from the server response;
   * query parameters are taken from the current status — never invented locally.
   */
  async function registerBundle(file?: File | null): Promise<boolean> {
    const current = status.value
    const bundle = file ?? selectedBundleFile.value
    if (!current?.desired || !bundle || registering.value) return false
    actionFailure.value = null
    notice.value = null

    if (bundle.size <= 0) {
      actionFailure.value = {
        message: '所选文件为空，无法登记。',
        recovery: '请选择实际导出的 .age 密文文件。',
        code: 'LW_PLATFORM_CONFIGURATION_BUNDLE_EMPTY',
      }
      return false
    }
    if (bundle.size > PLATFORM_CONFIGURATION_BUNDLE_MAX_BYTES) {
      actionFailure.value = {
        message: `所选文件 ${bundle.size} 字节超过服务端上限 ${PLATFORM_CONFIGURATION_BUNDLE_MAX_BYTES} 字节。`,
        recovery: '请使用不超过约 70 MiB 的加密 Bundle；所选文件已保留。',
        code: 'LW_PLATFORM_CONFIGURATION_BUNDLE_TOO_LARGE',
      }
      return false
    }

    selectedBundleFile.value = bundle
    const desiredRevision = current.desired.revision
    const configurationSha256 = current.desired.configurationSha256
    const fingerprint = fileFingerprint(bundle, desiredRevision, configurationSha256)
    registering.value = true
    try {
      const result = await registerPlatformConfigurationBundle({
        path: { revision: desiredRevision },
        query: {
          expectedRevision: current.revision,
          configurationSha256,
        },
        headers: {
          'Idempotency-Key': stableKey('register', fingerprint),
          'If-Match': etag.value || ifMatch(current.revision),
        } as Parameters<typeof registerPlatformConfigurationBundle>[0]['headers'],
        body: bundle,
      })
      if (result.error || !result.data) {
        if (isRevisionConflict(result.error)) {
          await handleConflict()
          actionFailure.value = {
            message: '配置 revision 已变化，Bundle 登记未完成。',
            recovery: '所选 .age 文件已保留。比较最新配置后，若 desired 未变可重新登记；若已保存新 desired 需重新导出并选择对应文件。',
            code: extractProblemDetails(result.error)?.diagnosticCode ?? 'LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT',
            requestId: extractProblemDetails(result.error)?.requestId,
            status: extractProblemDetails(result.error)?.status,
          }
          return false
        }
        actionFailure.value = failureFrom(result.error, '登记加密 Bundle 失败。', '所选文件已保留；可用相同文件与原幂等键重试，不要重新 seal。')
        return false
      }
      registerKey = null
      conflict.value = null
      const accepted = acceptStatus(result.data, readEtag(result as ResponseLike, result.data.revision))
      if (!accepted) {
        notice.value = '收到的是较早登记请求的回放结果，已保留更新的服务端事实。'
        await load({ keepDraft: true })
        return true
      }
      const registered = result.data.registeredBundle
      notice.value = registered
        ? `已登记 Bundle（密文 SHA-256 ${registered.encryptedBundleSha256.slice(0, 12)}…，${registered.encryptedBytes} 字节）。登记只保存可查询事实，不是批准或部署；可用当前 Status 提交应用请求。`
        : 'Bundle 登记已完成。登记不是批准或部署。'
      return true
    } finally {
      registering.value = false
    }
  }

  /** Record an apply intent using the current Status fence after a successful registration. */
  async function requestApply(): Promise<boolean> {
    const current = status.value
    if (!current?.desired || applying.value) return false
    actionFailure.value = null
    notice.value = null
    if (!current.registeredBundle || current.registeredBundle.desiredRevision !== current.desired.revision) {
      actionFailure.value = {
        message: '当前 desired 尚无有效的 Bundle 登记。',
        recovery: '请先选择实际 .age 文件完成登记，再用登记成功后的服务端事实提交应用请求。',
        code: 'LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED',
      }
      return false
    }
    const body = {
      expectedRevision: current.revision,
      desiredRevision: current.desired.revision,
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
          await handleConflict()
          actionFailure.value = {
            ...failureFrom(result.error, '配置 revision 已变化，应用请求未记录。', ''),
            recovery: '所选文件与草稿已保留。刷新比较最新 desired 与登记后重新提交。',
          }
          return false
        }
        actionFailure.value = failureFrom(result.error, '记录应用请求失败。', '确认已完成 Bundle 登记后重试。')
        return false
      }
      applyKey = null
      const accepted = acceptStatus(result.data, readEtag(result as ResponseLike, result.data.revision))
      if (!accepted) {
        notice.value = '收到的是较早应用请求的回放结果，已保留更新的服务端事实。'
        await load({ keepDraft: true })
        return true
      }
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
    registering,
    applying,
    actionFailure,
    conflict,
    notice,
    selectedBundleFile,
    registeredBundle,
    canApply,
    applyHistory,
    historyLoading,
    historyFailure,
    load,
    submitUpdate,
    rebaseDraftOnLatest,
    discardDraftForLatest,
    resetDraftFromServer,
    selectBundleFile,
    registerBundle,
    requestApply,
    loadPreviousApply,
    refreshApply,
    readVersion,
  })
}
