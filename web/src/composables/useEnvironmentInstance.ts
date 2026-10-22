import { getCurrentScope, onScopeDispose, reactive, ref, watch, type Ref } from 'vue'
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
  return makeDiagnostic(problem?.diagnosticCode ?? code, problem?.detail ?? message, problem?.retryable ?? false, problem)
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
  const requestItems = Array.isArray(requestResult.value.data)
    ? requestResult.value.data
    : requestResult.value.data?.items
  if (!Array.isArray(requestItems) || !Array.isArray(leaseResult.value.data)) {
    return {
      kind: 'error',
      diagnostic: makeDiagnostic('ENVIRONMENT_RESOURCE_CONTEXT_INVALID', '资源申请状态返回了无法识别的响应，请返回资源申请页检查。', false),
    }
  }

  const matchingRequests = requestItems.filter((request) => (
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

const ENVIRONMENT_POLL_INTERVAL_MS = 3000
const SETTLED_OBSERVED_STATES = new Set<EnvironmentInstanceSchema['observedState']>(['ready', 'stopped', 'failed', 'deleted'])
const ACTIVE_OPERATION_STATES = new Set<EnvironmentInstanceSchema['operation']['state']>(['accepted', 'running', 'cancelling'])

/** An environment still moving between states; a settled one only changes after a new command. */
export function environmentIsTransitioning(data: EnvironmentInstanceSchema): boolean {
  return !SETTLED_OBSERVED_STATES.has(data.observedState) || ACTIVE_OPERATION_STATES.has(data.operation.state)
}

/**
 * Reads one environment and keeps reading it only while it is transitioning
 * or its approved handoff is still pending. Settled environments, errors and
 * hidden tabs stop the timer; returning to the tab reads once and resumes only
 * if the environment is still moving. Any later `load()` (for example after a
 * lifecycle command) re-arms the timer from the fresh server state.
 */
export function useEnvironmentInstance(
  environmentId: Ref<string | undefined>,
  handoffProjectId: Ref<string | undefined> = ref(undefined),
) {
  const instance = ref<AsyncState<EnvironmentInstanceSchema>>({ kind: 'idle' })
  /** True while the environment needs watching; the timer itself also pauses on hidden tabs. */
  const polling = ref(false)
  let timer: ReturnType<typeof setTimeout> | null = null
  let pendingEnvironmentId: string | null = null
  let pendingDeadline = 0
  // Every load gets a generation; only the newest one may write state or re-arm the timer.
  let loadGeneration = 0

  async function load() {
    const generation = ++loadGeneration
    const id = environmentId.value
    const handoffProject = handoffProjectId.value
    if (!id) {
      pendingEnvironmentId = null
      pendingDeadline = 0
      instance.value = { kind: 'idle' }
      return
    }
    // An older or redirected read must not overwrite the current one or restart polling.
    const superseded = () => generation !== loadGeneration
      || environmentId.value !== id
      || handoffProjectId.value !== handoffProject
    if (instance.value.kind !== 'success') {
      instance.value = { kind: 'loading', message: '加载环境状态…' }
    }
    const result = await getEnvironment({ path: { environmentId: id } })
    if (superseded()) return
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      const handoffPending = handoffProject
        && result.response?.status === 404
        && problem?.diagnosticCode === 'LW_ENVIRONMENT_NOT_FOUND'
      if (handoffPending) {
        const resourceState = await readHandoffResourceState(handoffProject, id)
        if (superseded()) return
        if (resourceState.kind === 'error') {
          pendingEnvironmentId = null
          pendingDeadline = 0
          instance.value = { kind: 'error', diagnostic: resourceState.diagnostic }
          polling.value = false
          schedule()
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
            schedule()
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
          schedule()
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
          problem,
        ),
      }
      polling.value = false
      schedule()
      return
    }
    pendingEnvironmentId = null
    pendingDeadline = 0
    instance.value = { kind: 'success', data: result.data }
    polling.value = environmentIsTransitioning(result.data)
    schedule()
  }

  function clearTimer() {
    if (timer) {
      clearTimeout(timer)
      timer = null
    }
  }

  function pageHidden(): boolean {
    return typeof document !== 'undefined' && document.visibilityState === 'hidden'
  }

  function schedule() {
    clearTimer()
    if (!polling.value || !environmentId.value || pageHidden()) return
    timer = setTimeout(() => {
      timer = null
      void load()
    }, ENVIRONMENT_POLL_INTERVAL_MS)
  }

  function startPolling() {
    polling.value = true
    pendingEnvironmentId = null
    pendingDeadline = 0
    clearTimer()
    void load()
  }

  function stopPolling() {
    polling.value = false
    loadGeneration += 1
    clearTimer()
  }

  function onVisibilityChange() {
    if (pageHidden()) {
      clearTimer()
      return
    }
    if (polling.value && environmentId.value) void load()
  }
  if (typeof document !== 'undefined') document.addEventListener('visibilitychange', onVisibilityChange)
  if (getCurrentScope()) {
    onScopeDispose(() => {
      loadGeneration += 1
      clearTimer()
      if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', onVisibilityChange)
    })
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
