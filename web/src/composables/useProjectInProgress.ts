import { onScopeDispose, reactive, ref, watch, type Ref } from 'vue'
import { getProjectAgentTaskProgress, listEnvironments, listProjectAgentRuns, listProjectResourceRequests } from '@/generated/contracts'
import type { AgentRunHistoryItem, AgentRunTaskProgress, EnvironmentSummary, ResourceRequestSchema } from '@/generated/contracts'
import { diagnosticFromError, extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { collectCursorPages } from '@/utils/cursorPagination'
import { collectTaskProgress } from '@/utils/taskProgress'

/** Section state; `hidden` means the server denied this read for the account, so the section is not shown. */
export type InProgressSection<T> = AsyncState<T[]> | { kind: 'hidden' }

/** Current attempt per track of one open run; an error affects only that run's row. */
export type AgentRunProgressState =
  | { kind: 'loading' }
  | { kind: 'ready'; progress: AgentRunTaskProgress }
  | { kind: 'error'; diagnostic: DiagnosticViewModel }

/** At most this many task-progress reads are in flight at once, however many runs are open. */
export const AGENT_PROGRESS_MAX_CONCURRENCY = 4

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
  /** Keyed by run id; only runs of `projectId` that are still open appear here. */
  const agentProgress = ref<Record<string, AgentRunProgressState>>({})
  /** The server has more rows than the first page read here; the section says so instead of implying completeness. */
  const environmentsTruncated = ref(false)
  const agentRunsTruncated = ref(false)
  const loadedAt = ref<Date | null>(null)
  const loading = ref(false)
  let generation = 0
  /** Project whose runs `agentProgress` describes; a different project starts from an empty map. */
  let progressProjectId: string | null = null
  /** Sequence of the latest requested progress read per run; only that read may write its row. */
  const progressReadSequence = new Map<string, number>()
  /**
   * Slots shared by every task-progress read of this composable (batch loads of any
   * project or generation, and single-row retries). A read whose result is no longer
   * wanted keeps its slot until the request settles, so the bound always holds.
   */
  let progressSlotsInUse = 0
  const progressSlotWaiters: Array<() => void> = []

  async function loadEnvironments(id: string, current: number) {
    environments.value = { kind: 'loading', message: '加载运行中的环境…' }
    environmentsTruncated.value = false
    try {
      const result = await collectCursorPages<EnvironmentSummary>((cursor) => listEnvironments({ query: { projectId: id, desiredState: 'running', limit: 50, ...(cursor ? { cursor } : {}) } }))
      if (current !== generation) return
      const items = result.items.filter((item) => item.projectId === id && item.observedState !== 'deleted')
      environments.value = items.length ? { kind: 'success', data: items } : { kind: 'empty' }
    } catch (error) {
      if (current !== generation) return
      environments.value = denied(error)
        ? { kind: 'hidden' }
        : { kind: 'error', diagnostic: diagnosticFromError(error, 'HOME_ENVIRONMENTS_LOAD_FAILED', '加载运行中的环境失败。') }
    }
  }

  async function loadAgentRuns(id: string, current: number) {
    agentRuns.value = { kind: 'loading', message: '加载未完成的任务…' }
    agentRunsTruncated.value = false
    try {
      const result = await collectCursorPages<AgentRunHistoryItem>((cursor) => listProjectAgentRuns({
        path: { projectId: id },
        query: { state: [...OPEN_AGENT_RUN_STATES], limit: 50, ...(cursor ? { cursor } : {}) },
      }))
      if (current !== generation) return
      const items = result.items.filter((item) => item.projectId === id && isOpenAgentRun(item))
      agentRuns.value = items.length ? { kind: 'success', data: items } : { kind: 'empty' }
      await loadAgentProgress(id, items, current)
    } catch (error) {
      if (current !== generation) return
      agentRuns.value = denied(error)
        ? { kind: 'hidden' }
        : { kind: 'error', diagnostic: diagnosticFromError(error, 'HOME_AGENT_RUNS_LOAD_FAILED', '加载未完成的任务失败。') }
    }
  }

  function progressError(error: unknown): AgentRunProgressState {
    return { kind: 'error', diagnostic: diagnosticFromError(error, 'HOME_AGENT_PROGRESS_LOAD_FAILED', '读取任务进度失败。') }
  }

  async function readAgentProgress(id: string, runId: string): Promise<AgentRunProgressState> {
    try {
      const result = await collectTaskProgress((cursor) => getProjectAgentTaskProgress({ path: { projectId: id, runId }, query: { limit: 100, ...(cursor ? { cursor } : {}) } }))
      if (!result.data) return progressError(result.error)
      if (result.data.projectId !== id || result.data.runId !== runId) {
        return {
          kind: 'error',
          diagnostic: makeDiagnostic('HOME_AGENT_PROGRESS_STALE_CONTEXT', '任务进度返回了其他项目或任务的数据，已停止显示。', true),
        }
      }
      return { kind: 'ready', progress: result.data }
    } catch (error) {
      return progressError(error)
    }
  }

  function acquireProgressSlot(): Promise<void> {
    if (progressSlotsInUse < AGENT_PROGRESS_MAX_CONCURRENCY) {
      progressSlotsInUse += 1
      return Promise.resolve()
    }
    // Promise.withResolvers is outside the project's ES2020 lib.
    return new Promise((resolve) => { progressSlotWaiters.push(resolve) })
  }

  /** Hands the slot straight to the next waiter so nobody can take it in between. */
  function releaseProgressSlot() {
    const next = progressSlotWaiters.shift()
    if (next) next()
    else progressSlotsInUse -= 1
  }

  /**
   * Reads one run's progress through the shared slots and writes it only if it
   * is still the latest read requested for that run in the current generation.
   * A read superseded while it waited for a slot is never sent.
   */
  async function refreshRunProgress(id: string, runId: string, current: number) {
    const sequence = (progressReadSequence.get(runId) ?? 0) + 1
    progressReadSequence.set(runId, sequence)
    const wanted = () => current === generation && progressReadSequence.get(runId) === sequence && runId in agentProgress.value
    await acquireProgressSlot()
    try {
      if (!wanted()) return
      const state = await readAgentProgress(id, runId)
      if (wanted()) agentProgress.value = { ...agentProgress.value, [runId]: state }
    } finally {
      releaseProgressSlot()
    }
  }

  /**
   * Reads the current attempt of each open run. Rows already read keep their
   * last reading until the new one arrives; rows that closed are dropped along
   * with their read sequence. A failed read only marks that run.
   */
  async function loadAgentProgress(id: string, runs: AgentRunHistoryItem[], current: number) {
    const previous = agentProgress.value
    agentProgress.value = Object.fromEntries(runs.map((run) => [run.id, previous[run.id] ?? { kind: 'loading' } as const]))
    // A pruned run's in-flight read is still rejected by its older generation.
    for (const runId of progressReadSequence.keys()) {
      if (!(runId in agentProgress.value)) progressReadSequence.delete(runId)
    }
    await Promise.all(runs.map((run) => refreshRunProgress(id, run.id, current)))
  }

  /** Re-reads one run after its row reported a failure; other rows are untouched. */
  async function retryAgentProgress(runId: string) {
    const id = projectId.value
    if (!id || id !== progressProjectId || !(runId in agentProgress.value)) return
    agentProgress.value = { ...agentProgress.value, [runId]: { kind: 'loading' } }
    await refreshRunProgress(id, runId, generation)
  }

  async function loadResourceRequests(id: string, current: number) {
    resourceRequests.value = { kind: 'loading', message: '加载待处理事项…' }
    try {
      const result = await collectCursorPages<ResourceRequestSchema>((cursor) => listProjectResourceRequests({
        path: { projectId: id },
        query: { state: [...OPEN_RESOURCE_REQUEST_STATES, 'active'], limit: 50, ...(cursor ? { cursor } : {}) },
      }))
      if (current !== generation) return
      const items = result.items.filter((item) => item.projectId === id && isOpenResourceRequest(item))
      resourceRequests.value = items.length ? { kind: 'success', data: items } : { kind: 'empty' }
    } catch (error) {
      if (current !== generation) return
      resourceRequests.value = denied(error)
        ? { kind: 'hidden' }
        : { kind: 'error', diagnostic: diagnosticFromError(error, 'HOME_RESOURCE_REQUESTS_LOAD_FAILED', '加载资源申请失败。') }
    }
  }

  async function load() {
    const id = projectId.value
    const current = ++generation
    if (id !== progressProjectId) {
      agentProgress.value = {}
      // Sequences restart per project; reads from the previous project fail the generation check.
      progressReadSequence.clear()
      progressProjectId = id
    }
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

  return reactive({
    environments,
    agentRuns,
    agentProgress,
    resourceRequests,
    environmentsTruncated,
    agentRunsTruncated,
    loadedAt,
    loading,
    load,
    retryAgentProgress,
  })
}
