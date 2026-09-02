import { computed, reactive, ref, onMounted } from 'vue'
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

export function useResourceApproval() {
  const requests = ref<AsyncState<ResourceRequestSchema[]>>({ kind: 'idle' })
  const leases = ref<AsyncState<ResourceLeaseSchema[]>>({ kind: 'idle' })
  const selectedRequestId = ref<string | null>(null)
  const selectedLeaseId = ref<string | null>(null)
  const acting = ref<string | null>(null)
  const outcome = ref<ActionOutcome | null>(null)

  async function load() {
    requests.value = { kind: 'loading', message: '加载资源申请…' }
    leases.value = { kind: 'loading', message: '加载 Lease…' }
    const [requestResult, leaseResult] = await Promise.all([
      listResourceRequests({}),
      listResourceLeases({}),
    ])
    if (requestResult.error) {
      requests.value = { kind: 'error', diagnostic: errorDiagnostic(requestResult.error, 'RESOURCE_REQUEST_LIST_FAILED', '加载资源申请失败') }
    } else if (requestResult.data.length === 0) {
      requests.value = { kind: 'empty' }
    } else {
      requests.value = { kind: 'success', data: requestResult.data }
    }
    if (leaseResult.error) {
      leases.value = { kind: 'error', diagnostic: errorDiagnostic(leaseResult.error, 'RESOURCE_LEASE_LIST_FAILED', '加载 Lease 失败') }
    } else if (leaseResult.data.length === 0) {
      leases.value = { kind: 'empty' }
    } else {
      leases.value = { kind: 'success', data: leaseResult.data }
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

  /** Reads the authoritative revision immediately before a fenced mutation. */
  async function latestRequestRevision(requestId: string): Promise<number | DiagnosticViewModel> {
    const result = await getResourceRequest({ path: { requestId } })
    if (result.error) {
      return errorDiagnostic(result.error, 'RESOURCE_REQUEST_LOAD_FAILED', '读取资源申请最新 revision 失败')
    }
    return result.data.revision
  }

  async function latestLeaseRevision(leaseId: string): Promise<number | DiagnosticViewModel> {
    const result = await getResourceLease({ path: { leaseId } })
    if (result.error) {
      return errorDiagnostic(result.error, 'RESOURCE_LEASE_LOAD_FAILED', '读取 Lease 最新 revision 失败')
    }
    return result.data.revision
  }

  async function runRequestAction(kind: RequestActionKind, requestId: string, payload: ApprovePayload): Promise<boolean> {
    if (acting.value) return false
    acting.value = kind
    outcome.value = null
    try {
      const revision = await latestRequestRevision(requestId)
      if (typeof revision !== 'number') {
        outcome.value = { kind: 'error', diagnostic: revision }
        return false
      }
      const headers = { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(revision) }
      const path = { requestId }
      const result = kind === 'approve' || kind === 'resize'
        ? await (kind === 'approve' ? approveResourceRequest : resizeAndApproveResourceRequest)({
            path,
            headers,
            body: {
              expectedRevision: revision,
              providerBinding: payload.providerBinding,
              resources: payload.resources,
              durationSeconds: payload.durationSeconds,
              reason: payload.reason,
            },
          })
        : await (kind === 'reject' ? rejectResourceRequest : retryResourceRequest)({
            path,
            headers,
            body: { expectedRevision: revision, reason: payload.reason },
          })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'RESOURCE_REQUEST_ACTION_FAILED', '资源申请操作失败') }
        return false
      }
      const successCode = {
        approve: 'RESOURCE_REQUEST_APPROVED',
        resize: 'RESOURCE_REQUEST_RESIZED_AND_APPROVED',
        reject: 'RESOURCE_REQUEST_REJECTED',
        retry: 'RESOURCE_REQUEST_RETRIED',
      }[kind]
      outcome.value = {
        kind: 'success',
        diagnostic: makeDiagnostic(successCode, `操作已接受，当前 revision rev-${result.data.revision}。`, false),
      }
      await load()
      return true
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
      return true
    } finally {
      acting.value = null
    }
  }

  onMounted(load)

  return reactive({
    requests,
    leases,
    selectedRequestId,
    selectedLeaseId,
    selectedRequest,
    selectedLease,
    requestResources,
    acting,
    outcome,
    load,
    selectRequest,
    selectLease,
    runRequestAction,
    renewLease,
    revokeLease,
  })
}
