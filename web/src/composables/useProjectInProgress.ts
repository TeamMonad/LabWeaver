import { onScopeDispose, reactive, ref, watch, type Ref } from 'vue'
import { listEnvironments, listProjectAgentRuns, listProjectResourceRequests } from '@/generated/contracts'
import type { AgentRunHistoryItem, EnvironmentSummary, ResourceRequestSchema } from '@/generated/contracts'
import { diagnosticFromError, extractProblemDetails, type AsyncState } from '@/types/async'

/** Section state; `hidden` means the server denied this read for the account, so the section is not shown. */
export type InProgressSection<T> = AsyncState<T[]> | { kind: 'hidden' }

const OPEN_AGENT_RUN_STATES = new Set<AgentRunHistoryItem['state']>(['requested', 'running', 'awaiting_approval', 'cancelling'])
const OPEN_RESOURCE_REQUEST_STATES = new Set<ResourceRequestSchema['state']>(['reviewing', 'allocating', 'expiring'])

export function isOpenAgentRun(item: AgentRunHistoryItem): boolean {
  return OPEN_AGENT_RUN_STATES.has(item.state)
}

export function isOpenResourceRequest(item: ResourceRequestSchema): boolean {
  return OPEN_RESOURCE_REQUEST_STATES.has(item.state)
    || (item.state === 'active' && item.diagnosticCode === 'LW_RESOURCE_WORK_ALLOCATION_BLOCKED')
}

function denied(error: unknown): boolean {
  return extractProblemDetails(error)?.status === 403
}

/**
 * Read-only projection of what is in progress in one project, assembled from
 * the existing domain reads. It filters server state only; lifecycle and
 * approvals stay with their owning services and pages.
 *
 * Loads when the project changes, on demand, and when a hidden tab becomes
 * visible again. It never polls in the background.
 */
export function useProjectInProgress(projectId: Ref<string | null>) {
  const environments = ref<InProgressSection<EnvironmentSummary>>({ kind: 'idle' })
  const agentRuns = ref<InProgressSection<AgentRunHistoryItem>>({ kind: 'idle' })
  const resourceRequests = ref<InProgressSection<ResourceRequestSchema>>({ kind: 'idle' })
  /** The server has more rows than the first page read here; the section says so instead of implying completeness. */
  const environmentsTruncated = ref(false)
  const agentRunsTruncated = ref(false)
  const loadedAt = ref<Date | null>(null)
  const loading = ref(false)
  let generation = 0

  async function loadEnvironments(id: string, current: number) {
    environments.value = { kind: 'loading', message: '加载运行中的环境…' }
    environmentsTruncated.value = false
    const result = await listEnvironments({ query: { projectId: id, desiredState: 'running', limit: 50 } })
    if (current !== generation) return
    if (result.error) {
      environments.value = denied(result.error)
        ? { kind: 'hidden' }
        : { kind: 'error', diagnostic: diagnosticFromError(result.error, 'HOME_ENVIRONMENTS_LOAD_FAILED', '加载运行中的环境失败。') }
      return
    }
    environmentsTruncated.value = Boolean(result.data.nextCursor)
    const items = result.data.items.filter((item) => item.projectId === id && item.observedState !== 'deleted')
    environments.value = items.length ? { kind: 'success', data: items } : { kind: 'empty' }
  }

  async function loadAgentRuns(id: string, current: number) {
    agentRuns.value = { kind: 'loading', message: '加载未完成的任务…' }
    agentRunsTruncated.value = false
    const result = await listProjectAgentRuns({ path: { projectId: id }, query: { page: 1, pageSize: 50 } })
    if (current !== generation) return
    if (result.error) {
      agentRuns.value = denied(result.error)
        ? { kind: 'hidden' }
        : { kind: 'error', diagnostic: diagnosticFromError(result.error, 'HOME_AGENT_RUNS_LOAD_FAILED', '加载未完成的任务失败。') }
      return
    }
    agentRunsTruncated.value = result.data.hasMore
    const items = result.data.items.filter((item) => item.projectId === id && isOpenAgentRun(item))
    agentRuns.value = items.length ? { kind: 'success', data: items } : { kind: 'empty' }
  }

  async function loadResourceRequests(id: string, current: number) {
    resourceRequests.value = { kind: 'loading', message: '加载待处理事项…' }
    const result = await listProjectResourceRequests({ path: { projectId: id } })
    if (current !== generation) return
    if (result.error) {
      resourceRequests.value = denied(result.error)
        ? { kind: 'hidden' }
        : { kind: 'error', diagnostic: diagnosticFromError(result.error, 'HOME_RESOURCE_REQUESTS_LOAD_FAILED', '加载资源申请失败。') }
      return
    }
    const items = result.data.filter((item) => item.projectId === id && isOpenResourceRequest(item))
    resourceRequests.value = items.length ? { kind: 'success', data: items } : { kind: 'empty' }
  }

  async function load() {
    const id = projectId.value
    const current = ++generation
    if (!id) {
      environments.value = { kind: 'idle' }
      agentRuns.value = { kind: 'idle' }
      resourceRequests.value = { kind: 'idle' }
      loadedAt.value = null
      return
    }
    loading.value = true
    try {
      await Promise.all([loadEnvironments(id, current), loadAgentRuns(id, current), loadResourceRequests(id, current)])
      if (current === generation) loadedAt.value = new Date()
    } finally {
      if (current === generation) loading.value = false
    }
  }

  let hiddenSinceLoad = false
  function onVisibilityChange() {
    if (document.visibilityState === 'hidden') {
      hiddenSinceLoad = true
      return
    }
    if (hiddenSinceLoad) {
      hiddenSinceLoad = false
      void load()
    }
  }
  if (typeof document !== 'undefined') document.addEventListener('visibilitychange', onVisibilityChange)

  watch(projectId, () => void load(), { immediate: true })
  onScopeDispose(() => {
    generation += 1
    if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', onVisibilityChange)
  })

  return reactive({ environments, agentRuns, resourceRequests, environmentsTruncated, agentRunsTruncated, loadedAt, loading, load })
}
