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
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'
import {
  canonicalJson,
  cloneConfiguration,
  parseRequestYaml,
} from '@/utils/platformConfiguration'

/** Maximum encrypted Bundle upload accepted by Control (70 MiB). */
export const PLATFORM_CONFIGURATION_BUNDLE_MAX_BYTES = 73_400_320

/** Local fail-closed code; mirrors the Bundle/Helm rejection of settings without an execution-side consumer. */
export const POLICY_UNSUPPORTED_CODE = 'LW_PLATFORM_CONFIGURATION_POLICY_UNSUPPORTED'

export interface PlatformConfigurationConflict {
  /** The server facts read after the 409/412; the draft is kept untouched. */
  latest: PlatformConfigurationStatusSchema
  etag: string
  /** Configuration the rejected draft was edited from. */
  base: PlatformConfigurationDesired | null
}

export type PlatformConfigurationAction = 'save' | 'register' | 'apply'

type ResponseLike = { response?: { headers?: Record<string, unknown> } }

function readEtag(result: ResponseLike, revision: number): string {
  const headers = result.response?.headers
  const raw = headers?.etag ?? headers?.ETag
  return typeof raw === 'string' && raw ? raw : ifMatch(revision)
}

/**
 * Chinese reason for every Control diagnostic of this API. Control's problem
 * `detail` is a generic English boundary sentence, so it is never shown as
 * the reason; the code and requestId stay in the collapsed banner details.
 */
const REASONS: Record<string, string> = {
  LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT: '配置已被其他管理员更新，本次提交未生效。',
  LW_IDEMPOTENCY_CONFLICT: '同一请求标识已用于不同内容，服务端拒绝复用，本次提交未生效。',
  LW_OPERATION_IN_PROGRESS: '相同请求仍在服务端处理中。',
  LW_PLATFORM_CONFIGURATION_APPLY_IN_PROGRESS: '上一次应用尝试尚未结束，不能提交新的应用请求。',
  LW_PLATFORM_CONFIGURATION_APPLY_CONFLICT: '应用尝试的状态已变化，本次请求未记录。',
  LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED: '当前 desired 没有有效的 Bundle 登记，服务端拒绝应用请求。',
  LW_PLATFORM_CONFIGURATION_BUNDLE_TOO_LARGE: '密文文件超过服务端 70 MiB 上限，登记未完成。',
  LW_PLATFORM_CONFIGURATION_CONTENT_TYPE_INVALID: '服务端不接受该请求格式。',
  LW_PLATFORM_CONFIGURATION_INVALID: '服务端拒绝了提交内容：结构、字段或文件格式无效。',
  LW_PLATFORM_CONFIGURATION_LIFECYCLE_INVALID: '生命周期设置超出允许范围。',
  LW_PLATFORM_CONFIGURATION_EXECUTION_LOCATION_INVALID: '特权执行位置无效。',
  LW_PLATFORM_CONFIGURATION_PREAUTHORIZATION_INVALID: '预授权超出平台允许清单或 scope 重复。',
  LW_PLATFORM_CONFIGURATION_NOT_FOUND: '请求的配置版本、登记或应用尝试不存在。',
  LW_PLATFORM_CONFIGURATION_POLICY_UNSUPPORTED: '当前策略字段尚无执行侧消费者，服务端拒绝保存或登记。',
  LW_PLATFORM_CONFIGURATION_PERSISTENCE_FAILED: '平台配置存储暂时不可用。',
}

const NEXT_STEPS: Record<string, string> = {
  LW_IDEMPOTENCY_CONFLICT: '已为下一次提交换用新的请求标识；刷新确认当前事实后再提交。',
  LW_OPERATION_IN_PROGRESS: '稍后按原样重试；服务端按同一请求标识回放结果，不会重复执行。',
  LW_PLATFORM_CONFIGURATION_APPLY_IN_PROGRESS: '等待执行者报告终态后刷新，再决定是否提交新的尝试。',
  LW_PLATFORM_CONFIGURATION_APPLY_CONFLICT: '刷新读取最新应用尝试后再决定下一步。',
  LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED: '先选择与当前 desired 对应的实际 .age 文件完成登记，再提交应用请求。',
  LW_PLATFORM_CONFIGURATION_BUNDLE_TOO_LARGE: '使用不超过约 70 MiB 的加密 Bundle；所选文件已保留。',
  LW_PLATFORM_CONFIGURATION_LIFECYCLE_INVALID: '留存与提醒须在 1 秒到 365 天之间，提醒最多 16 个且不能重复，抢占宽限为 1–300 秒；草稿已保留。',
  LW_PLATFORM_CONFIGURATION_EXECUTION_LOCATION_INVALID: '允许特权时必须声明专用节点池（合法的 nodeSelector 与 taint）；不允许特权时不能声明位置。草稿已保留。',
  LW_PLATFORM_CONFIGURATION_PREAUTHORIZATION_INVALID: '预授权只能使用平台允许清单内的 capability，且同一 scope 只能出现一次；草稿已保留。',
  LW_PLATFORM_CONFIGURATION_POLICY_UNSUPPORTED: '将具体字段恢复为当前可部署的默认值后再保存；执行侧支持由 #207 等后续消费者提供。',
  LW_PLATFORM_CONFIGURATION_NOT_FOUND: '刷新页面读取当前事实。',
  LW_PLATFORM_CONFIGURATION_PERSISTENCE_FAILED: '稍后按原样重试；草稿与所选文件已保留。',
}

function failureFrom(error: unknown, fallback: string, actionNextStep?: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  const code = problem?.diagnosticCode ?? 'LW_PLATFORM_CONFIGURATION_REQUEST_FAILED'
  const diagnostic = makeDiagnostic(code, REASONS[code] ?? fallback, problem?.retryable ?? !problem, problem)
  if (problem?.status === 403) {
    diagnostic.message = '只有平台管理员可以读取或修改平台配置。'
    diagnostic.nextStep = '请使用平台管理员账号登录后再试。'
  } else if (NEXT_STEPS[code]) {
    diagnostic.nextStep = NEXT_STEPS[code]
  } else if (actionNextStep && problem?.status !== 401) {
    diagnostic.nextStep = actionNextStep
  } else if (!problem) {
    diagnostic.nextStep = '无法连接平台服务。请检查网络后按原样重试；草稿与所选文件已保留。'
  }
  return diagnostic
}

/**
 * Administrator platform configuration over the Control SDK.
 *
 * The server is the only writer of revisions, desired versions, registration
 * digests and apply facts. This composable keeps one editable draft copied from
 * the server, submits it with the observed revision/ETag and a stable
 * idempotency key, keeps the draft and selected Bundle file on any failure, and
 * never advances local state from an older replayed response. Control replays a
 * completed request before checking If-Match, so a retried request may return
 * an earlier snapshot; it is accepted only if it is not older than shown facts.
 */
export function usePlatformConfiguration() {
  const status = ref<PlatformConfigurationStatusSchema | null>(null)
  const etag = ref('')
  const loadState = ref<'idle' | 'loading' | 'ready' | 'error'>('idle')
  const loadFailure = ref<DiagnosticViewModel | null>(null)

  const draft = ref<PlatformConfigurationDesired | null>(null)
  /** Server configuration the draft was copied from (desired, or defaults when unconfigured). */
  const draftBase = ref<PlatformConfigurationDesired | null>(null)
  /** Revision the next update must name in expectedRevision / If-Match. */
  const baseRevision = ref(0)
  const baseEtag = ref('')

  const saving = ref(false)
  const registering = ref(false)
  const applying = ref(false)
  const actionFailure = ref<{ action: PlatformConfigurationAction; diagnostic: DiagnosticViewModel } | null>(null)
  const conflict = ref<PlatformConfigurationConflict | null>(null)
  const notice = ref<string | null>(null)

  /** Selected ciphertext file for register; kept across 409/412 and network retries. */
  const selectedBundleFile = ref<File | null>(null)

  const applyHistory = ref<PlatformConfigurationApply[]>([])
  const historyLoading = ref(false)
  const historyFailure = ref<DiagnosticViewModel | null>(null)

  // Idempotency keys stay stable while the same request is retried, and change with the request.
  const keys: Record<PlatformConfigurationAction, { fingerprint: string; key: string } | null> = { save: null, register: null, apply: null }

  function stableKey(slot: PlatformConfigurationAction, fingerprint: string): string {
    const current = keys[slot]
    if (current && current.fingerprint === fingerprint) return current.key
    const next = { fingerprint, key: idempotencyKey() }
    keys[slot] = next
    return next.key
  }

  function fail(action: PlatformConfigurationAction, diagnostic: DiagnosticViewModel): false {
    actionFailure.value = { action, diagnostic }
    return false
  }

  const serverConfiguration = computed<PlatformConfigurationDesired | null>(() =>
    status.value?.desired?.configuration ?? status.value?.defaults ?? null,
  )

  const draftDirty = computed(() =>
    draft.value !== null && draftBase.value !== null && canonicalJson(draft.value) !== canonicalJson(draftBase.value),
  )


  const registeredBundle = computed(() => status.value?.registeredBundle ?? null)

  const canRegister = computed(() => Boolean(
    status.value?.desired
    && selectedBundleFile.value
    && !registering.value
    && conflict.value === null,
  ))

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

  // Every status read takes a generation. A read response is accepted only if no newer read was
  // issued and no mutation response was accepted after it was issued; every response must also
  // not be older (by aggregate revision) than the facts already shown.
  let issuedRead = 0
  let staleReadsUpTo = 0

  type StatusSource = { read: number } | 'mutation' | 'conflict-snapshot'

  function readIsCurrent(generation: number): boolean {
    return generation === issuedRead && generation > staleReadsUpTo
  }

  /** The only writer of status, ETag and apply history. Returns false when the response is discarded. */
  function acceptStatus(next: PlatformConfigurationStatusSchema, nextEtag: string, source: StatusSource): boolean {
    if (typeof source === 'object' && !readIsCurrent(source.read)) return false
    if (status.value && next.revision < status.value.revision) return false
    status.value = next
    etag.value = nextEtag
    // A newer mutation result makes every read still in flight stale.
    if (source === 'mutation') staleReadsUpTo = issuedRead
    // Keep the loaded previousApplyId chain while its head is still the latest attempt.
    const latest = next.latestApply
    if (!latest) applyHistory.value = []
    else if (applyHistory.value[0]?.id === latest.id) applyHistory.value = [latest, ...applyHistory.value.slice(1)]
    else applyHistory.value = [latest]
    return true
  }

  async function fetchStatus(): Promise<{ generation: number } & ({ data: PlatformConfigurationStatusSchema; etag: string } | { failure: DiagnosticViewModel })> {
    const generation = ++issuedRead
    const result = await getPlatformConfiguration()
    if (result.error || !result.data) {
      return { generation, failure: failureFrom(result.error, '读取平台配置失败。', '稍后刷新重试。') }
    }
    return { generation, data: result.data, etag: readEtag(result as ResponseLike, result.data.revision) }
  }

  async function load(options: { keepDraft?: boolean } = {}): Promise<void> {
    loadState.value = 'loading'
    const outcome = await fetchStatus()
    if (!readIsCurrent(outcome.generation)) {
      // Superseded by a newer read (which owns loadState) or by an accepted mutation result.
      if (outcome.generation === issuedRead) loadState.value = status.value ? 'ready' : 'error'
      return
    }
    if ('failure' in outcome) {
      loadFailure.value = outcome.failure
      loadState.value = status.value ? 'ready' : 'error'
      return
    }
    if (acceptStatus(outcome.data, outcome.etag, { read: outcome.generation })) {
      loadFailure.value = null
      if (!options.keepDraft || draft.value === null) resetDraftFromServer()
    }
    loadState.value = 'ready'
  }

  async function handleConflict(action: PlatformConfigurationAction, error: unknown, recovery: string): Promise<void> {
    const diagnostic = failureFrom(error, REASONS.LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT)
    diagnostic.message = REASONS.LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT
    diagnostic.nextStep = recovery
    const outcome = await fetchStatus()
    if ('failure' in outcome) {
      diagnostic.nextStep = `${recovery} 重新读取最新配置也失败了，请稍后点击刷新。`
      fail(action, diagnostic)
      return
    }
    fail(action, diagnostic)
    // A superseded or older read never becomes the comparison target; the newer shown facts do.
    const fresh = readIsCurrent(outcome.generation) && (!status.value || outcome.data.revision >= status.value.revision)
    const latest = fresh ? { data: outcome.data, etag: outcome.etag } : status.value ? { data: status.value, etag: etag.value } : null
    if (!latest) return
    conflict.value = { latest: latest.data, etag: latest.etag, base: draftBase.value ? cloneConfiguration(draftBase.value) : null }
  }

  /** Handle a rejected mutation: revision conflicts open the comparison, idempotency conflicts drop the key. */
  async function rejectMutation(action: PlatformConfigurationAction, error: unknown, fallback: string, conflictRecovery: string, actionNextStep?: string): Promise<false> {
    const problem = extractProblemDetails(error)
    // Only a revision fence failure means another administrator changed the configuration; other 409s keep their own meaning.
    if (problem?.status === 412 || problem?.diagnosticCode === 'LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT') {
      await handleConflict(action, error, conflictRecovery)
      return false
    }
    if (problem?.diagnosticCode === 'LW_IDEMPOTENCY_CONFLICT') keys[action] = null
    return fail(action, failureFrom(error, fallback, actionNextStep))
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
        return fail('save', makeDiagnostic('LW_PLATFORM_CONFIGURATION_YAML_INVALID', parsed.message, false))
      }
      if (parsed.request.expectedRevision !== baseRevision.value) {
        const diagnostic = makeDiagnostic(
          'LW_PLATFORM_CONFIGURATION_REVISION_MISMATCH',
          `YAML 中的 expectedRevision（${parsed.request.expectedRevision}）与当前编辑基线 revision ${baseRevision.value} 不一致。`,
          false,
        )
        diagnostic.nextStep = '将 expectedRevision 改为当前基线，或刷新后在最新配置上重新编辑；内容未发送。'
        return fail('save', diagnostic)
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
        return await rejectMutation('save', result.error, '保存平台配置失败。', '草稿未提交且已保留。比较下方差异后选择继续方式。', '草稿已保留，按提示修改后重新提交。')
      }
      keys.save = null
      conflict.value = null
      const accepted = acceptStatus(result.data, readEtag(result as ResponseLike, result.data.revision), 'mutation')
      if (!accepted) {
        notice.value = '收到的是较早请求的回放结果，已保留更新的服务端事实。'
        await load({ keepDraft: true })
        return true
      }
      resetDraftFromServer()
      // New desired clears the registration pointer server-side; drop stale register idempotency.
      keys.register = null
      notice.value = `已保存为 desired revision ${result.data.desired?.revision ?? result.data.revision}。保存会清空当前 Bundle 登记，需要重新导出并登记 .age 文件后才能提交应用；保存不等于部署。`
      return true
    } finally {
      saving.value = false
    }
  }

  /** Keep the draft and edit on top of the newer server revision after a conflict. */
  function rebaseDraftOnLatest(): void {
    if (!conflict.value) return
    // The snapshot is accepted only if nothing newer arrived meanwhile; the base is whatever is now shown.
    acceptStatus(conflict.value.latest, conflict.value.etag, 'conflict-snapshot')
    const source = serverConfiguration.value
    draftBase.value = source ? cloneConfiguration(source) : null
    baseRevision.value = status.value?.revision ?? 0
    baseEtag.value = etag.value
    conflict.value = null
    actionFailure.value = null
    keys.save = null
    keys.register = null
  }

  /** Drop the draft and continue from the newer server configuration. */
  function discardDraftForLatest(): void {
    if (!conflict.value) return
    acceptStatus(conflict.value.latest, conflict.value.etag, 'conflict-snapshot')
    conflict.value = null
    actionFailure.value = null
    keys.save = null
    keys.register = null
    resetDraftFromServer()
  }

  function selectBundleFile(file: File | null): void {
    selectedBundleFile.value = file
    keys.register = null
    if (actionFailure.value?.action === 'register') actionFailure.value = null
  }

  /**
   * Upload the selected .age ciphertext. Digests come from the server response;
   * query parameters are taken from the current status — never invented locally.
   */
  async function registerBundle(): Promise<boolean> {
    const current = status.value
    const bundle = selectedBundleFile.value
    if (!current?.desired || !bundle || registering.value) return false
    actionFailure.value = null
    notice.value = null

    if (bundle.size <= 0) {
      const diagnostic = makeDiagnostic('LW_PLATFORM_CONFIGURATION_BUNDLE_EMPTY', '所选文件为空，无法登记。', false)
      diagnostic.nextStep = '请选择实际导出的 .age 密文文件。'
      return fail('register', diagnostic)
    }
    if (bundle.size > PLATFORM_CONFIGURATION_BUNDLE_MAX_BYTES) {
      const diagnostic = makeDiagnostic(
        'LW_PLATFORM_CONFIGURATION_BUNDLE_TOO_LARGE',
        `所选文件 ${bundle.size} 字节超过服务端上限 ${PLATFORM_CONFIGURATION_BUNDLE_MAX_BYTES} 字节。`,
        false,
      )
      diagnostic.nextStep = NEXT_STEPS.LW_PLATFORM_CONFIGURATION_BUNDLE_TOO_LARGE
      return fail('register', diagnostic)
    }

    const desiredRevision = current.desired.revision
    const configurationSha256 = current.desired.configurationSha256
    registering.value = true
    try {
      const result = await registerPlatformConfigurationBundle({
        path: { revision: desiredRevision },
        query: { expectedRevision: current.revision, configurationSha256 },
        headers: {
          'Idempotency-Key': stableKey('register', `register:${desiredRevision}:${configurationSha256}:${bundle.name}:${bundle.size}:${bundle.lastModified}`),
          'If-Match': etag.value || ifMatch(current.revision),
        } as Parameters<typeof registerPlatformConfigurationBundle>[0]['headers'],
        body: bundle,
      })
      if (result.error || !result.data) {
        return await rejectMutation(
          'register',
          result.error,
          '登记加密 Bundle 失败。',
          '所选 .age 文件已保留。若最新 desired 未变可重新登记；若已保存新 desired，需重新导出并选择对应文件。',
          '所选文件已保留；网络失败时用同一文件按原样重试，不要重新 seal。',
        )
      }
      keys.register = null
      conflict.value = null
      const accepted = acceptStatus(result.data, readEtag(result as ResponseLike, result.data.revision), 'mutation')
      if (!accepted) {
        notice.value = '收到的是较早登记请求的回放结果，已保留更新的服务端事实。'
        await load({ keepDraft: true })
        return true
      }
      const registered = result.data.registeredBundle
      notice.value = registered
        ? `已登记 Bundle（密文 SHA-256 ${registered.encryptedBundleSha256.slice(0, 12)}…，${registered.encryptedBytes} 字节）。登记只保存可查询事实，不是批准或部署。`
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
      const diagnostic = makeDiagnostic('LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED', '当前 desired 尚无有效的 Bundle 登记。', false)
      diagnostic.nextStep = NEXT_STEPS.LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED
      return fail('apply', diagnostic)
    }
    const body = { expectedRevision: current.revision, desiredRevision: current.desired.revision }
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
        return await rejectMutation('apply', result.error, '记录应用请求失败。', '草稿与所选文件已保留。比较最新 desired 与登记后重新提交。')
      }
      keys.apply = null
      const accepted = acceptStatus(result.data, readEtag(result as ResponseLike, result.data.revision), 'mutation')
      if (!accepted) {
        notice.value = '收到的是较早应用请求的回放结果，已保留更新的服务端事实。'
        await load({ keepDraft: true })
        return true
      }
      notice.value = '应用请求已记录（202）。这只表示请求已登记，部署由获准执行者执行，并以其报告的阶段为准。'
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
      // The chain may have been replaced (new latest attempt) while this read was in flight.
      if (applyHistory.value[applyHistory.value.length - 1]?.id !== last.id) return
      applyHistory.value = [...applyHistory.value, result.data]
    } finally {
      historyLoading.value = false
    }
  }

  async function refreshApply(applyId: string): Promise<void> {
    historyFailure.value = null
    const result = await getPlatformConfigurationApply({ path: { applyId } })
    if (result.error || !result.data) {
      historyFailure.value = failureFrom(result.error, '刷新应用尝试失败。', '稍后重试。')
      return
    }
    const fresh = result.data
    // Attempt revisions only grow; an older response never replaces a newer attempt fact.
    applyHistory.value = applyHistory.value.map((item) => (item.id === applyId && fresh.revision >= item.revision ? fresh : item))
  }

  async function readVersion(revision: number): Promise<{ data: PlatformConfigurationVersionSchema } | { failure: DiagnosticViewModel }> {
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
    canRegister,
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
