import { onScopeDispose, reactive, ref, watch, type Ref } from 'vue'
import {
  cancelResourceRequest,
  createResourceRequest,
  getResourceLease,
  getResourceRequest,
  listProjectResourceLeases,
  listProjectResourceRequests,
  renewResourceLease,
  revokeResourceLease,
} from '@/generated/contracts'
import type { ResourceLeaseSchema, ResourceRequestSchema, WorkloadResources } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey as makeIdempotencyKey, ifMatch } from '@/utils/format'

function errorDiagnostic(error: unknown, code: string, message: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? code, problem?.detail ?? message, problem?.retryable ?? true)
}

async function listProjectRequests(projectId: string) {
  return listProjectResourceRequests({ path: { projectId } })
}

async function listProjectLeases(projectId: string) {
  return listProjectResourceLeases({ path: { projectId } })
}

const TRANSITIONAL_REQUEST_STATES = new Set<ResourceRequestSchema['state']>(['reviewing', 'allocating', 'active', 'expiring'])
const TRANSITIONAL_LEASE_STATES = new Set<ResourceLeaseSchema['state']>(['allocating', 'active', 'expiring'])

export interface ResourceMutationOutcome {
  kind: 'success' | 'error'
  diagnostic: DiagnosticViewModel
}

export function useProjectResources(projectId: Ref<string | null>) {
  const requests = ref<AsyncState<ResourceRequestSchema[]>>({ kind: 'idle' })
  const leases = ref<AsyncState<ResourceLeaseSchema[]>>({ kind: 'idle' })
  const acting = ref<string | null>(null)
  const outcome = ref<ResourceMutationOutcome | null>(null)
  let pollTimer: ReturnType<typeof setTimeout> | null = null
  let loadGeneration = 0

  function stopPolling() {
    if (pollTimer) {
      clearTimeout(pollTimer)
      pollTimer = null
    }
  }

  function schedulePoll() {
    stopPolling()
    const requestPending = requests.value.kind === 'success' && requests.value.data.some((item) => TRANSITIONAL_REQUEST_STATES.has(item.state))
    const leasePending = leases.value.kind === 'success' && leases.value.data.some((item) => TRANSITIONAL_LEASE_STATES.has(item.state))
    if ((!requestPending && !leasePending) || (typeof document !== 'undefined' && document.visibilityState === 'hidden')) return
    const generation = loadGeneration
    pollTimer = setTimeout(() => {
      pollTimer = null
      if (generation !== loadGeneration) return
      void load(true)
    }, 5000)
  }

  async function load(silent = false) {
    const id = projectId.value
    const generation = ++loadGeneration
    if (!id) {
      requests.value = { kind: 'idle' }
      leases.value = { kind: 'idle' }
      stopPolling()
      return
    }
    if (!silent) {
      requests.value = { kind: 'loading', message: '加载项目资源申请…' }
      leases.value = { kind: 'loading', message: '加载项目 Lease…' }
    }
    const [requestResult, leaseResult] = await Promise.allSettled([listProjectRequests(id), listProjectLeases(id)])
    if (generation !== loadGeneration) return

    if (requestResult.status === 'rejected') {
      requests.value = { kind: 'error', diagnostic: errorDiagnostic(requestResult.reason, 'PROJECT_RESOURCE_REQUESTS_LOAD_FAILED', '加载项目资源申请失败') }
    } else if (requestResult.value.error) {
      requests.value = { kind: 'error', diagnostic: errorDiagnostic(requestResult.value.error, 'PROJECT_RESOURCE_REQUESTS_LOAD_FAILED', '加载项目资源申请失败') }
    } else if (!Array.isArray(requestResult.value.data)) {
      requests.value = { kind: 'error', diagnostic: makeDiagnostic('PROJECT_RESOURCE_REQUESTS_INVALID', 'Resource 返回了无法识别的资源申请响应。', false) }
    } else {
      requests.value = requestResult.value.data.length > 0 ? { kind: 'success', data: requestResult.value.data } : { kind: 'empty' }
    }

    if (leaseResult.status === 'rejected') {
      leases.value = { kind: 'error', diagnostic: errorDiagnostic(leaseResult.reason, 'PROJECT_RESOURCE_LEASES_LOAD_FAILED', '加载项目 Lease 失败') }
    } else if (leaseResult.value.error) {
      leases.value = { kind: 'error', diagnostic: errorDiagnostic(leaseResult.value.error, 'PROJECT_RESOURCE_LEASES_LOAD_FAILED', '加载项目 Lease 失败') }
    } else if (!Array.isArray(leaseResult.value.data)) {
      leases.value = { kind: 'error', diagnostic: makeDiagnostic('PROJECT_RESOURCE_LEASES_INVALID', 'Resource 返回了无法识别的 Lease 响应。', false) }
    } else {
      leases.value = leaseResult.value.data.length > 0 ? { kind: 'success', data: leaseResult.value.data } : { kind: 'empty' }
    }
    schedulePoll()
  }

  async function create(
    input: Omit<Parameters<typeof createResourceRequest>[0]['body'], 'projectId'> & { projectId?: string },
    options: { idempotencyKey?: string } = {},
  ): Promise<boolean> {
    const id = projectId.value
    if (!id || acting.value) return false
    acting.value = 'create'
    outcome.value = null
    try {
      const result = await createResourceRequest({
        headers: { 'Idempotency-Key': options.idempotencyKey ?? makeIdempotencyKey() },
        body: { ...input, projectId: id } as Parameters<typeof createResourceRequest>[0]['body'],
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'PROJECT_RESOURCE_REQUEST_CREATE_FAILED', '提交资源申请失败') }
        return false
      }
      outcome.value = { kind: 'success', diagnostic: makeDiagnostic('PROJECT_RESOURCE_REQUEST_ACCEPTED', `资源申请 ${result.data.requestId} 已提交，等待审批。`, false) }
      await load()
      return true
    } finally {
      acting.value = null
    }
  }

  async function latestRequest(id: string): Promise<ResourceRequestSchema | null> {
    const result = await getResourceRequest({ path: { requestId: id } })
    if (result.error) {
      outcome.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'PROJECT_RESOURCE_REQUEST_LOAD_FAILED', '读取资源申请最新状态失败') }
      return null
    }
    return result.data
  }

  async function cancel(id: string, reason: string): Promise<boolean> {
    if (acting.value) return false
    const current = await latestRequest(id)
    if (!current) return false
    acting.value = `cancel:${id}`
    outcome.value = null
    try {
      const result = await cancelResourceRequest({
        path: { requestId: id },
        headers: { 'Idempotency-Key': makeIdempotencyKey(), 'If-Match': ifMatch(current.revision) },
        body: { expectedRevision: current.revision, reason },
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'PROJECT_RESOURCE_REQUEST_CANCEL_FAILED', '取消资源申请失败') }
        return false
      }
      outcome.value = { kind: 'success', diagnostic: makeDiagnostic('PROJECT_RESOURCE_REQUEST_CANCEL_ACCEPTED', '取消资源申请已接受。', false) }
      await load()
      return true
    } finally {
      acting.value = null
    }
  }

  async function renew(lease: ResourceLeaseSchema, durationSeconds: number, reason: string): Promise<boolean> {
    if (acting.value) return false
    const latest = await getResourceLease({ path: { leaseId: lease.id } })
    if (latest.error) {
      outcome.value = { kind: 'error', diagnostic: errorDiagnostic(latest.error, 'PROJECT_RESOURCE_LEASE_LOAD_FAILED', '读取 Lease 最新状态失败') }
      return false
    }
    acting.value = `renew:${lease.id}`
    outcome.value = null
    try {
      const result = await renewResourceLease({
        path: { leaseId: lease.id },
        headers: { 'Idempotency-Key': makeIdempotencyKey(), 'If-Match': ifMatch(latest.data.revision) },
        body: { expectedRevision: latest.data.revision, durationSeconds, reason },
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'PROJECT_RESOURCE_LEASE_RENEW_FAILED', 'Lease 续期失败') }
        return false
      }
      outcome.value = { kind: 'success', diagnostic: makeDiagnostic('PROJECT_RESOURCE_LEASE_RENEWED', 'Lease 续期请求已接受。', false) }
      await load()
      return true
    } finally {
      acting.value = null
    }
  }

  async function reclaim(lease: ResourceLeaseSchema, reason: string): Promise<boolean> {
    if (acting.value) return false
    const latest = await getResourceLease({ path: { leaseId: lease.id } })
    if (latest.error) {
      outcome.value = { kind: 'error', diagnostic: errorDiagnostic(latest.error, 'PROJECT_RESOURCE_LEASE_LOAD_FAILED', '读取 Lease 最新状态失败') }
      return false
    }
    acting.value = `reclaim:${lease.id}`
    outcome.value = null
    try {
      const result = await revokeResourceLease({
        path: { leaseId: lease.id },
        headers: { 'Idempotency-Key': makeIdempotencyKey(), 'If-Match': ifMatch(latest.data.revision) },
        body: { expectedRevision: latest.data.revision, reason },
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'PROJECT_RESOURCE_LEASE_RECLAIM_FAILED', '回收 Lease 失败') }
        return false
      }
      outcome.value = { kind: 'success', diagnostic: makeDiagnostic('PROJECT_RESOURCE_LEASE_RECLAIMED', '回收请求已接受；实际释放完成后 Lease 才会终止。', false) }
      await load()
      return true
    } finally {
      acting.value = null
    }
  }

  function onVisibilityChange() {
    if (document.visibilityState === 'visible') schedulePoll()
    else stopPolling()
  }
  if (typeof document !== 'undefined') document.addEventListener('visibilitychange', onVisibilityChange)
  watch(projectId, () => {
    stopPolling()
    void load()
  }, { immediate: true })
  onScopeDispose(() => {
    loadGeneration += 1
    stopPolling()
    if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', onVisibilityChange)
  })

  return reactive({ requests, leases, acting, outcome, load, create, cancel, renew, reclaim, stopPolling })
}

export function resourceSummary(resources: WorkloadResources): string {
  const base = `${resources.cpuMillicores}m CPU · ${(resources.memoryBytes / 1024 ** 3).toFixed(1)} GiB 内存 · ${(resources.storageBytes / 1024 ** 3).toFixed(1)} GiB 存储`
  return resources.gpu ? `${base} · GPU ${resources.gpu.class} × ${resources.gpu.count}` : base
}
