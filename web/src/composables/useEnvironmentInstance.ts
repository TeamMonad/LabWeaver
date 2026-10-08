import { reactive, ref, watch, type Ref } from 'vue'
import { getEnvironment, listProjectResourceLeases, listProjectResourceRequests } from '@/generated/contracts'
import type { EnvironmentInstanceSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { resourceLeaseStateLabel, resourceRequestStateLabel } from '@/utils/stateLabels'

const ENVIRONMENT_HANDOFF_TIMEOUT_MS = 240_000
const RESOURCE_HANDOFF_STATES = new Set(['allocating', 'active', 'expiring'])
const TERMINAL_REQUEST_STATES = new Set(['expired', 'rejected', 'cancelled'])
const TERMINAL_LEASE_STATES = new Set(['expired', 'revoked'])

type HandoffResourceState =
  | { kind: 'pending' }
  | { kind: 'missing' }
  | { kind: 'error'; diagnostic: DiagnosticViewModel }

function resourceReadDiagnostic(error: unknown, code: string, message: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? code, problem?.detail ?? message, problem?.retryable ?? false)
}

async function readHandoffResourceState(projectId: string, environmentId: string): Promise<HandoffResourceState> {
  const [requestResult, leaseResult] = await Promise.allSettled([
    listProjectResourceRequests({ path: { projectId } }),
    listProjectResourceLeases({ path: { projectId } }),
  ])
  if (requestResult.status === 'rejected') {
    return {
      kind: 'error',
      diagnostic: resourceReadDiagnostic(requestResult.reason, 'ENVIRONMENT_RESOURCE_REQUESTS_READ_FAILED', '无法核对资源申请状态，请返回资源申请页检查。'),
    }
  }
  if (leaseResult.status === 'rejected') {
    return {
      kind: 'error',
      diagnostic: resourceReadDiagnostic(leaseResult.reason, 'ENVIRONMENT_RESOURCE_LEASES_READ_FAILED', '无法核对资源授权状态，请返回资源申请页检查。'),
    }
  }
  if (requestResult.value.error) {
    return {
      kind: 'error',
      diagnostic: resourceReadDiagnostic(requestResult.value.error, 'ENVIRONMENT_RESOURCE_REQUESTS_READ_FAILED', '无法核对资源申请状态，请返回资源申请页检查。'),
    }
  }
  if (leaseResult.value.error) {
    return {
      kind: 'error',
      diagnostic: resourceReadDiagnostic(leaseResult.value.error, 'ENVIRONMENT_RESOURCE_LEASES_READ_FAILED', '无法核对资源授权状态，请返回资源申请页检查。'),
    }
  }
  if (!Array.isArray(requestResult.value.data) || !Array.isArray(leaseResult.value.data)) {
    return {
      kind: 'error',
      diagnostic: makeDiagnostic('ENVIRONMENT_RESOURCE_CONTEXT_INVALID', '资源申请状态返回了无法识别的响应，请返回资源申请页检查。', false),
    }
  }

  const matchingRequests = requestResult.value.data.filter((request) => (
    request.projectId === projectId
    && request.target?.kind === 'environment'
    && request.target.environmentId === environmentId
  ))
  if (matchingRequests.length === 0) return { kind: 'missing' }

  let terminalRequest: (typeof matchingRequests)[number] | undefined
  for (const request of matchingRequests) {
    if (TERMINAL_REQUEST_STATES.has(request.state)) {
      terminalRequest = request
      continue
    }
    if (!RESOURCE_HANDOFF_STATES.has(request.state)) continue
    const lease = leaseResult.value.data.find((item) => item.requestId === request.id)
    if (!lease) continue
    if (TERMINAL_LEASE_STATES.has(lease.state)) {
      return {
        kind: 'error',
        diagnostic: makeDiagnostic(
          lease.revokeReasonCode ?? 'ENVIRONMENT_RESOURCE_LEASE_TERMINAL',
          `资源授权${resourceLeaseStateLabel(lease.state)}，请返回资源申请页处理后重试。`,
          false,
        ),
      }
    }
    if (RESOURCE_HANDOFF_STATES.has(lease.state)) return { kind: 'pending' }
  }
  if (terminalRequest) {
    return {
      kind: 'error',
      diagnostic: makeDiagnostic(
        terminalRequest.diagnosticCode ?? 'ENVIRONMENT_RESOURCE_REQUEST_TERMINAL',
        `资源申请${resourceRequestStateLabel(terminalRequest.state)}，请返回资源申请页处理后重试。`,
        false,
      ),
    }
  }
  return { kind: 'missing' }
}

export function useEnvironmentInstance(
  environmentId: Ref<string | undefined>,
  handoffProjectId: Ref<string | undefined> = ref(undefined),
) {
  const instance = ref<AsyncState<EnvironmentInstanceSchema>>({ kind: 'idle' })
  const polling = ref(false)
  let timer: ReturnType<typeof setTimeout> | null = null
  let pendingEnvironmentId: string | null = null
  let pendingDeadline = 0

  async function load() {
    const id = environmentId.value
    if (!id) {
      pendingEnvironmentId = null
      pendingDeadline = 0
      instance.value = { kind: 'idle' }
      return
    }
    if (instance.value.kind !== 'success') {
      instance.value = { kind: 'loading', message: '加载环境状态…' }
    }
    const result = await getEnvironment({ path: { environmentId: id } })
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      const handoffPending = handoffProjectId.value
        && result.response?.status === 404
        && problem?.diagnosticCode === 'LW_ENVIRONMENT_NOT_FOUND'
      if (handoffPending) {
        const resourceState = handoffProjectId.value
          ? await readHandoffResourceState(handoffProjectId.value, id)
          : { kind: 'missing' as const }
        if (resourceState.kind === 'error') {
          pendingEnvironmentId = null
          pendingDeadline = 0
          instance.value = { kind: 'error', diagnostic: resourceState.diagnostic }
          polling.value = false
          return
        }
        if (resourceState.kind !== 'pending') {
          pendingEnvironmentId = null
          pendingDeadline = 0
        } else {
          if (pendingEnvironmentId !== id || pendingDeadline === 0) {
            pendingEnvironmentId = id
            pendingDeadline = Date.now() + ENVIRONMENT_HANDOFF_TIMEOUT_MS
          }
          if (Date.now() < pendingDeadline) {
            polling.value = true
            instance.value = { kind: 'loading', message: '环境正在准备…' }
            return
          }
          instance.value = {
            kind: 'error',
            diagnostic: makeDiagnostic(
              'LW_ENVIRONMENT_HANDOFF_TIMEOUT',
              '环境在限定时间内仍未完成准备，请返回资源申请页检查申请和授权后重试。',
              true,
            ),
          }
          polling.value = false
          return
        }
      }
      pendingEnvironmentId = null
      pendingDeadline = 0
      instance.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'ENVIRONMENT_LOAD_FAILED',
          problem?.detail ?? '加载环境状态失败',
          problem?.retryable ?? true,
        ),
      }
      polling.value = false
      return
    }
    pendingEnvironmentId = null
    pendingDeadline = 0
    instance.value = { kind: 'success', data: result.data }
  }

  async function poll() {
    const id = environmentId.value
    if (!id || !polling.value) return
    await load()
    if (polling.value) {
      timer = setTimeout(poll, 3000)
    }
  }

  function startPolling() {
    polling.value = true
    pendingEnvironmentId = null
    pendingDeadline = 0
    if (timer) clearTimeout(timer)
    poll()
  }

  function stopPolling() {
    polling.value = false
    if (timer) {
      clearTimeout(timer)
      timer = null
    }
  }

  watch(
    [environmentId, handoffProjectId],
    ([id]) => {
      if (id) {
        startPolling()
      } else {
        stopPolling()
        instance.value = { kind: 'idle' }
      }
    },
    { immediate: true },
  )

  return reactive({ instance, polling, load, startPolling, stopPolling })
}
