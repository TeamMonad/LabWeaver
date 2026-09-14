import { computed, reactive, ref, onMounted, onScopeDispose } from 'vue'
import {
  approveResourceRequest,
  getResourceLease,
  getResourceRequest,
  listResourceLeases,
  listResourceRequests,
  rejectResourceRequest,
  renewResourceLease,
  resizeAndApproveResourceRequest,
  retryResourceRequest,
  revokeResourceLease,
} from '@/generated/contracts'
import type {
  ResourceLeaseSchema,
  ResourceRequestSchema,
  WorkloadResources,
} from '@/generated/contracts'
import { apiClient } from '@/api/client'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

function errorDiagnostic(err: unknown, fallbackCode: string, fallbackDetail: string): DiagnosticViewModel {
  const problem = extractProblemDetails(err)
  return makeDiagnostic(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackDetail, problem?.retryable ?? true)
}

export type RequestActionKind = 'approve' | 'resize' | 'reject' | 'retry'
export type LeaseActionKind = 'renew' | 'revoke'

export interface ApprovePayload {
  providerBinding: string
  resources: WorkloadResources
  durationSeconds: number
  reason: string
}

export interface ActionOutcome {
  kind: 'success' | 'error'
  diagnostic: DiagnosticViewModel
}

export interface RequestActionItem {
  requestId: string
  expectedRevision: number
  expectedFingerprint: string
  payload: ApprovePayload
}

export interface RequestActionSnapshot {
  expectedRevision: number
  expectedFingerprint: string
}

export interface BatchActionItemOutcome extends ActionOutcome {
  requestId: string
}

export interface BatchActionOutcome {
  kind: 'success' | 'partial' | 'error'
  items: BatchActionItemOutcome[]
}

export interface ResourceProviderOption {
  providerBinding: string
  catalogEntryCount: number
  gpuClasses: string[]
}

function parseProviderOptions(value: unknown): ResourceProviderOption[] {
  if (!Array.isArray(value)) throw new Error('GPU catalog response must be an array')
  const providers = new Map<string, { count: number; classes: Set<string> }>()
  for (const item of value) {
    if (!item || typeof item !== 'object') throw new Error('GPU catalog entry must be an object')
    const entry = item as Record<string, unknown>
    if (
      typeof entry.providerBinding !== 'string' ||
      !entry.providerBinding.trim() ||
      typeof entry.class !== 'string' ||
      !entry.class.trim() ||
      typeof entry.active !== 'boolean' ||
      entry.active !== true ||
      !Number.isSafeInteger(entry.capacityUnits) ||
      Number(entry.capacityUnits) <= 0
    ) {
      throw new Error('GPU catalog entry is invalid')
    }
    const current = providers.get(entry.providerBinding) ?? { count: 0, classes: new Set<string>() }
    current.count += 1
    current.classes.add(entry.class)
    providers.set(entry.providerBinding, current)
  }
  return Array.from(providers, ([providerBinding, value]) => ({
    providerBinding,
    catalogEntryCount: value.count,
    gpuClasses: Array.from(value.classes).sort(),
  })).sort((left, right) => left.providerBinding.localeCompare(right.providerBinding))
}

/** Stable request content used to fence a confirmation snapshot. */
export function requestFingerprint(request: ResourceRequestSchema): string {
  return JSON.stringify({
    id: request.id,
    generation: request.generation,
    requestKey: request.requestKey,
    requesterId: request.requesterId,
    courseId: request.courseId ?? null,
    projectId: request.projectId,
    target: request.target,
    requestedResources: request.requestedResources,
    requestedDurationSeconds: request.requestedDurationSeconds,
    state: request.state,
    revision: request.revision,
  })
}

export function useResourceApproval() {
  const requests = ref<AsyncState<ResourceRequestSchema[]>>({ kind: 'idle' })
  const leases = ref<AsyncState<ResourceLeaseSchema[]>>({ kind: 'idle' })
  const providerOptions = ref<AsyncState<ResourceProviderOption[]>>({ kind: 'idle' })
  const selectedRequestId = ref<string | null>(null)
  const selectedLeaseId = ref<string | null>(null)
  const acting = ref<string | null>(null)
  const outcome = ref<ActionOutcome | null>(null)
  const batchOutcome = ref<BatchActionOutcome | null>(null)
  const refreshDiagnostic = ref<DiagnosticViewModel | null>(null)

  async function load() {
    refreshDiagnostic.value = null
    requests.value = { kind: 'loading', message: '加载资源申请…' }
    leases.value = { kind: 'loading', message: '加载 Lease…' }
    providerOptions.value = { kind: 'loading', message: '加载 Resource 容量目录…' }
    const [requestResult, leaseResult, providerResult] = await Promise.allSettled([
      listResourceRequests({}),
      listResourceLeases({}),
      apiClient.get<unknown[], unknown>({ url: '/api/v1/resource/gpu-catalog' }),
    ])
    if (requestResult.status === 'rejected') {
      requests.value = { kind: 'error', diagnostic: errorDiagnostic(requestResult.reason, 'RESOURCE_REQUEST_LIST_FAILED', '加载资源申请失败') }
    } else if (requestResult.value.error) {
      requests.value = { kind: 'error', diagnostic: errorDiagnostic(requestResult.value.error, 'RESOURCE_REQUEST_LIST_FAILED', '加载资源申请失败') }
    } else if (requestResult.value.data.length === 0) {
      requests.value = { kind: 'empty' }
    } else {
      requests.value = { kind: 'success', data: requestResult.value.data }
    }
    if (leaseResult.status === 'rejected') {
      leases.value = { kind: 'error', diagnostic: errorDiagnostic(leaseResult.reason, 'RESOURCE_LEASE_LIST_FAILED', '加载 Lease 失败') }
    } else if (leaseResult.value.error) {
      leases.value = { kind: 'error', diagnostic: errorDiagnostic(leaseResult.value.error, 'RESOURCE_LEASE_LIST_FAILED', '加载 Lease 失败') }
    } else if (leaseResult.value.data.length === 0) {
      leases.value = { kind: 'empty' }
    } else {
      leases.value = { kind: 'success', data: leaseResult.value.data }
    }
    if (providerResult.status === 'rejected') {
      providerOptions.value = { kind: 'error', diagnostic: errorDiagnostic(providerResult.reason, 'GPU_CATALOG_LOAD_FAILED', '加载 Resource 容量目录失败') }
    } else if (providerResult.value.error) {
      providerOptions.value = { kind: 'error', diagnostic: errorDiagnostic(providerResult.value.error, 'GPU_CATALOG_LOAD_FAILED', '加载 Resource 容量目录失败') }
    } else {
      try {
        const options = parseProviderOptions(providerResult.value.data)
        providerOptions.value = options.length > 0 ? { kind: 'success', data: options } : { kind: 'empty' }
      } catch (error) {
        const diagnostic = makeDiagnostic('GPU_CATALOG_INVALID', error instanceof Error ? error.message : 'Resource 返回了无法识别的容量目录。', false)
        providerOptions.value = { kind: 'error', diagnostic }
      }
    }
  }

  const selectedRequest = computed<ResourceRequestSchema | null>(() => {
    if (requests.value.kind !== 'success' || !selectedRequestId.value) return null
    return requests.value.data.find((request) => request.id === selectedRequestId.value) ?? null
  })

  const selectedLease = computed<ResourceLeaseSchema | null>(() => {
    if (leases.value.kind !== 'success' || !selectedLeaseId.value) return null
    return leases.value.data.find((lease) => lease.id === selectedLeaseId.value) ?? null
  })

  const requestResources = computed<Map<string, WorkloadResources>>(() => {
    const map = new Map<string, WorkloadResources>()
    if (requests.value.kind === 'success') {
      for (const request of requests.value.data) {
        map.set(request.id, request.requestedResources)
      }
    }
    return map
  })

  function selectRequest(requestId: string) {
    selectedRequestId.value = requestId
    outcome.value = null
  }

  function selectLease(leaseId: string) {
    selectedLeaseId.value = leaseId
    outcome.value = null
  }

  async function latestRequest(requestId: string, snapshot: RequestActionSnapshot): Promise<ResourceRequestSchema | DiagnosticViewModel> {
    const result = await getResourceRequest({ path: { requestId } })
    if (result.error) {
      return errorDiagnostic(result.error, 'RESOURCE_REQUEST_LOAD_FAILED', '读取资源申请最新 revision 失败')
    }
    if (result.data.revision !== snapshot.expectedRevision || requestFingerprint(result.data) !== snapshot.expectedFingerprint) {
      return makeDiagnostic('RESOURCE_REQUEST_CHANGED_RESELECT', '资源申请在确认后已发生变化，请刷新列表并重新确认后再审批。', false)
    }
    if (requests.value.kind !== 'success') {
      return makeDiagnostic('RESOURCE_REQUEST_LIST_NOT_READY', '资源申请列表尚未完成刷新，请稍后重试。', true)
    }
    const listed = requests.value.data.find((request) => request.id === requestId)
    if (!listed) {
      return makeDiagnostic('RESOURCE_REQUEST_NOT_IN_CURRENT_LIST', '资源申请已不在当前列表中，请刷新后重新选择。', false)
    }
    if (requestFingerprint(listed) !== snapshot.expectedFingerprint) {
      return makeDiagnostic('RESOURCE_REQUEST_CHANGED_RESELECT', '资源申请在确认后已发生变化，请刷新列表并重新确认后再审批。', false)
    }
    return result.data
  }

  async function latestLeaseRevision(leaseId: string): Promise<number | DiagnosticViewModel> {
    const result = await getResourceLease({ path: { leaseId } })
    if (result.error) {
      return errorDiagnostic(result.error, 'RESOURCE_LEASE_LOAD_FAILED', '读取 Lease 最新 revision 失败')
    }
    return result.data.revision
  }

  async function performRequestAction(kind: RequestActionKind, item: RequestActionItem): Promise<ActionOutcome> {
    try {
      const latest = await latestRequest(item.requestId, item)
      if (typeof latest !== 'object' || !('revision' in latest)) {
        return { kind: 'error', diagnostic: latest }
      }
      const revision = latest.revision
      const allowedStates: Record<RequestActionKind, ResourceRequestSchema['state'][]> = {
        approve: ['reviewing'],
        resize: ['reviewing'],
        reject: ['reviewing'],
        retry: ['allocating'],
      }
      if (!allowedStates[kind].includes(latest.state)) {
        return {
          kind: 'error',
          diagnostic: makeDiagnostic(
            'RESOURCE_REQUEST_STATE_CHANGED',
            `资源申请当前状态为“${latest.state}”，已停止本次操作，请刷新后重新选择。`,
            false,
          ),
        }
      }
      const headers = { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(revision) }
      const path = { requestId: item.requestId }
      const result = kind === 'approve' || kind === 'resize'
        ? await (kind === 'approve' ? approveResourceRequest : resizeAndApproveResourceRequest)({
            path,
            headers,
            body: {
              expectedRevision: revision,
              providerBinding: item.payload.providerBinding,
              resources: item.payload.resources,
              durationSeconds: item.payload.durationSeconds,
              reason: item.payload.reason,
            },
          })
        : await (kind === 'reject' ? rejectResourceRequest : retryResourceRequest)({
            path,
            headers,
          body: { expectedRevision: revision, reason: item.payload.reason },
          })
      if (result.error) {
        return { kind: 'error', diagnostic: errorDiagnostic(result.error, 'RESOURCE_REQUEST_ACTION_FAILED', '资源申请操作失败') }
      }
      const successCode = {
        approve: 'RESOURCE_REQUEST_APPROVED',
        resize: 'RESOURCE_REQUEST_RESIZED_AND_APPROVED',
        reject: 'RESOURCE_REQUEST_REJECTED',
        retry: 'RESOURCE_REQUEST_RETRIED',
      }[kind]
      return {
        kind: 'success',
        diagnostic: makeDiagnostic(successCode, `操作已接受，当前 revision rev-${result.data.revision}。`, false),
      }
    } catch (error) {
      return { kind: 'error', diagnostic: errorDiagnostic(error, 'RESOURCE_REQUEST_ACTION_FAILED', '资源申请操作失败') }
    }
  }

  async function runRequestAction(
    kind: RequestActionKind,
    requestId: string,
    payload: ApprovePayload,
    snapshot: RequestActionSnapshot,
  ): Promise<boolean> {
    if (acting.value) return false
    acting.value = kind
    outcome.value = null
    batchOutcome.value = null
    try {
      const result = await performRequestAction(kind, {
        requestId,
        ...snapshot,
        payload,
      })
      outcome.value = result
      if (result.kind === 'success') {
        await load()
        schedulePoll()
      }
      return result.kind === 'success'
    } finally {
      acting.value = null
    }
  }

  /**
   * Run explicitly selected request mutations one at a time. Each request
   * obtains its own latest revision and idempotency key immediately before
   * the mutation. A failed item does not hide the outcome of other items.
   */
  async function runRequestActions(kind: RequestActionKind, items: RequestActionItem[]): Promise<BatchActionOutcome> {
    if (acting.value || items.length === 0) {
      const diagnostic = makeDiagnostic(
        'RESOURCE_REQUEST_BATCH_NOT_READY',
        items.length === 0 ? '未选择资源申请。' : '已有资源申请操作正在执行。',
        false,
      )
      const blocked: BatchActionOutcome = {
        kind: 'error',
        items: items.map(({ requestId }) => ({ requestId, kind: 'error', diagnostic })),
      }
      batchOutcome.value = blocked
      return blocked
    }

    acting.value = `batch:${kind}`
    outcome.value = null
    batchOutcome.value = null
    const itemOutcomes: BatchActionItemOutcome[] = []
    try {
      for (const item of items) {
        const result = await performRequestAction(kind, item)
        itemOutcomes.push({ requestId: item.requestId, ...result })
      }

      const succeeded = itemOutcomes.filter((item) => item.kind === 'success').length
      const batch: BatchActionOutcome = {
        kind: succeeded === itemOutcomes.length ? 'success' : succeeded > 0 ? 'partial' : 'error',
        items: itemOutcomes,
      }
      batchOutcome.value = batch
      await load()
      schedulePoll()
      return batch
    } finally {
      acting.value = null
    }
  }

  async function renewLease(leaseId: string, durationSeconds: number, reason: string): Promise<boolean> {
    if (acting.value) return false
    acting.value = 'renew'
    outcome.value = null
    try {
      const revision = await latestLeaseRevision(leaseId)
      if (typeof revision !== 'number') {
        outcome.value = { kind: 'error', diagnostic: revision }
        return false
      }
      const result = await renewResourceLease({
        path: { leaseId },
        headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(revision) },
        body: { expectedRevision: revision, durationSeconds, reason },
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'RESOURCE_LEASE_RENEW_FAILED', 'Lease 续期失败') }
        return false
      }
      outcome.value = {
        kind: 'success',
        diagnostic: makeDiagnostic('RESOURCE_LEASE_RENEWED', `Lease 已续期，当前 revision rev-${result.data.revision}。`, false),
      }
      await load()
      schedulePoll()
      return true
    } finally {
      acting.value = null
    }
  }

  async function revokeLease(leaseId: string, reason: string): Promise<boolean> {
    if (acting.value) return false
    acting.value = 'revoke'
    outcome.value = null
    try {
      const revision = await latestLeaseRevision(leaseId)
      if (typeof revision !== 'number') {
        outcome.value = { kind: 'error', diagnostic: revision }
        return false
      }
      const result = await revokeResourceLease({
        path: { leaseId },
        headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(revision) },
        body: { expectedRevision: revision, reason },
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'RESOURCE_LEASE_REVOKE_FAILED', 'Lease 撤销失败') }
        return false
      }
      outcome.value = {
        kind: 'success',
        diagnostic: makeDiagnostic('RESOURCE_LEASE_REVOKED', `Lease 已撤销，当前 revision rev-${result.data.revision}。`, false),
      }
      await load()
      schedulePoll()
      return true
    } finally {
      acting.value = null
    }
  }

  onMounted(load)

  /**
   * Approval outcomes progress asynchronously (Approved → Allocating → Active,
   * leases → Expiring → Expired). Poll quietly while any row is transitional
   * so admins see state advance without manual reloads. Pauses when the page
   * is hidden and stops on scope disposal.
   */
  const REQUEST_TRANSITIONAL_STATES = new Set(['submitted', 'policy_checked', 'reviewing', 'allocating'])
  const LEASE_TRANSITIONAL_STATES = new Set(['active', 'expiring'])
  const POLL_INTERVAL_MS = 5000
  let pollTimer: ReturnType<typeof setTimeout> | null = null

  function stopPolling() {
    if (pollTimer) {
      clearTimeout(pollTimer)
      pollTimer = null
    }
  }

  async function refreshSilently() {
    const [requestResult, leaseResult, providerResult] = await Promise.allSettled([
      listResourceRequests({}),
      listResourceLeases({}),
      apiClient.get<unknown[], unknown>({ url: '/api/v1/resource/gpu-catalog' }),
    ])
    const failures: DiagnosticViewModel[] = []
    if (requestResult.status === 'fulfilled' && !requestResult.value.error) {
      requests.value = { kind: 'success', data: requestResult.value.data }
    } else {
      failures.push(errorDiagnostic(
        requestResult.status === 'rejected' ? requestResult.reason : requestResult.value.error,
        'RESOURCE_REQUEST_REFRESH_FAILED',
        '自动刷新资源申请失败',
      ))
    }
    if (leaseResult.status === 'fulfilled' && !leaseResult.value.error) {
      leases.value = { kind: 'success', data: leaseResult.value.data }
    } else {
      failures.push(errorDiagnostic(
        leaseResult.status === 'rejected' ? leaseResult.reason : leaseResult.value.error,
        'RESOURCE_LEASE_REFRESH_FAILED',
        '自动刷新 Lease 失败',
      ))
    }
    if (providerResult.status === 'fulfilled' && !providerResult.value.error) {
      try {
        const options = parseProviderOptions(providerResult.value.data)
        providerOptions.value = options.length > 0 ? { kind: 'success', data: options } : { kind: 'empty' }
      } catch (error) {
        const diagnostic = makeDiagnostic('GPU_CATALOG_INVALID', error instanceof Error ? error.message : 'Resource 返回了无法识别的容量目录。', false)
        providerOptions.value = { kind: 'error', diagnostic }
        failures.push(diagnostic)
      }
    } else {
      failures.push(errorDiagnostic(
        providerResult.status === 'rejected' ? providerResult.reason : providerResult.value.error,
        'GPU_CATALOG_REFRESH_FAILED',
        '自动刷新 Resource 容量目录失败',
      ))
    }
    refreshDiagnostic.value = failures.length > 0
      ? makeDiagnostic(
          'RESOURCE_REFRESH_PARTIAL',
          `自动刷新未完成，页面可能仍显示上次成功数据。${failures.map((failure) => `${failure.code}: ${failure.message}`).join('；')}`,
          true,
        )
      : null
    schedulePoll()
  }

  function schedulePoll() {
    stopPolling()
    if (requests.value.kind !== 'success' && leases.value.kind !== 'success') return
    const requestPending =
      requests.value.kind === 'success' &&
      requests.value.data.some((request) => REQUEST_TRANSITIONAL_STATES.has(request.state))
    const leasePending =
      leases.value.kind === 'success' &&
      leases.value.data.some((lease) => LEASE_TRANSITIONAL_STATES.has(lease.state))
    if (!requestPending && !leasePending) return
    if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return
    pollTimer = setTimeout(() => void refreshSilently(), POLL_INTERVAL_MS)
  }

  function onVisibilityChange() {
    if (document.visibilityState === 'visible') schedulePoll()
    else stopPolling()
  }

  if (typeof document !== 'undefined') {
    document.addEventListener('visibilitychange', onVisibilityChange)
    onScopeDispose(() => {
      document.removeEventListener('visibilitychange', onVisibilityChange)
      stopPolling()
    })
  }

  // schedulePoll is driven from refreshSilently/load completions.
  const baseLoad = load
  async function loadWithPolling() {
    await baseLoad()
    schedulePoll()
  }

  return reactive({
    requests,
    leases,
    providerOptions,
    selectedRequestId,
    selectedLeaseId,
    selectedRequest,
    selectedLease,
    requestResources,
    acting,
    outcome,
    batchOutcome,
    refreshDiagnostic,
    load: loadWithPolling,
    selectRequest,
    selectLease,
    runRequestAction,
    runRequestActions,
    renewLease,
    revokeLease,
  })
}
