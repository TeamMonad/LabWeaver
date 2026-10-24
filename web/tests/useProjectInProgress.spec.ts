import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { effectScope, nextTick, ref, type EffectScope } from 'vue'
import { flushPromises } from '@vue/test-utils'
import { getProjectAgentTaskProgress, listEnvironments, listProjectAgentRuns, listProjectResourceRequests } from '@/generated/contracts'
import { useProjectInProgress } from '@/composables/useProjectInProgress'

vi.mock('@/generated/contracts', () => ({
  getProjectAgentTaskProgress: vi.fn(),
  listEnvironments: vi.fn(),
  listProjectAgentRuns: vi.fn(),
  listProjectResourceRequests: vi.fn(),
}))

type ProgressOptions = { path: { projectId: string; runId: string } }
type ListOptions = { path: { projectId: string } }

const ok = <T,>(data: T) => ({ data, error: undefined }) as never

function openRun(projectId: string, id: string) {
  return { id, projectId, state: 'running', purpose: { kind: 'work_configuration' }, createdAt: '2026-10-01T00:00:00Z', updatedAt: '2026-10-01T00:00:00Z' }
}

function progressFor(options: ProgressOptions, phase = 'executing') {
  return {
    projectId: options.path.projectId, runId: options.path.runId, page: 1, pageSize: 1, hasMore: false, revision: 1, state: 'running',
    current: [{ track: 'work_configuration', progress: { attempt: 1, phase, nextSteps: [] } }],
    history: [],
  }
}

const unavailable = () => ({
  data: undefined,
  error: { status: 503, diagnosticCode: 'LW_AGENT_UNAVAILABLE', detail: 'Agent 服务暂时不可用。', requestId: 'req-503', retryable: true, title: '', type: 'about:blank', instance: '/' },
}) as never

interface PendingRead {
  options: ProgressOptions
  settle: (response: unknown) => void
}

/** Holds every progress read until the test settles it, and records the peak number in flight. */
function controlProgressReads() {
  const pending: PendingRead[] = []
  const stats = { inFlight: 0, peak: 0 }
  vi.mocked(getProjectAgentTaskProgress).mockImplementation(((options: ProgressOptions) => new Promise((resolve) => {
    stats.inFlight += 1
    stats.peak = Math.max(stats.peak, stats.inFlight)
    pending.push({
      options,
      settle: (response) => {
        stats.inFlight -= 1
        resolve(response)
      },
    })
  })) as never)
  return { pending, stats }
}

function phaseOf(state: unknown): string | undefined {
  const reading = state as { kind: string; progress?: { current: Array<{ progress: { phase: string } }> } } | undefined
  return reading?.kind === 'ready' ? reading.progress?.current[0]?.progress.phase : reading?.kind
}

let scope: EffectScope | null = null

afterEach(() => {
  scope?.stop()
  scope = null
})

describe('useProjectInProgress task progress', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(listEnvironments).mockResolvedValue(ok({ items: [], nextCursor: null }))
    vi.mocked(listProjectResourceRequests).mockResolvedValue(ok({ items: [], nextCursor: null }))
    vi.mocked(listProjectAgentRuns).mockImplementation((async (options: ListOptions) => ok({
      items: [openRun(options.path.projectId, `run-of-${options.path.projectId}`)],
      nextCursor: null,
    })) as never)
  })

  it('never shows the previous project\'s progress after switching, even when its response arrives late', async () => {
    const late: Array<() => void> = []
    vi.mocked(getProjectAgentTaskProgress).mockImplementation(((options: ProgressOptions) => {
      if (options.path.projectId === 'project-1') {
        return new Promise((resolve) => { late.push(() => resolve(ok(progressFor(options)))) })
      }
      return Promise.resolve(ok(progressFor(options)))
    }) as never)
    const projectId = ref<string | null>('project-1')
    scope = effectScope()
    const inProgress = scope.run(() => useProjectInProgress(projectId))
    if (!inProgress) throw new Error('composable did not start')
    await flushPromises()
    expect(inProgress.agentProgress).toEqual({ 'run-of-project-1': { kind: 'loading' } })

    projectId.value = 'project-2'
    await nextTick()
    // Cleared as soon as the project changes, before project-2 is read.
    expect(inProgress.agentProgress).not.toHaveProperty('run-of-project-1')

    await flushPromises()
    late.forEach((settle) => settle())
    await flushPromises()

    expect(Object.keys(inProgress.agentProgress)).toEqual(['run-of-project-2'])
    expect(inProgress.agentProgress['run-of-project-2']).toMatchObject({ kind: 'ready', progress: { projectId: 'project-2' } })
  })

  it('keeps an earlier reading of the same project while a reload is in flight and drops runs that closed', async () => {
    vi.mocked(getProjectAgentTaskProgress).mockImplementation((async (options: ProgressOptions) => ok(progressFor(options))) as never)
    const projectId = ref<string | null>('project-1')
    scope = effectScope()
    const inProgress = scope.run(() => useProjectInProgress(projectId))
    if (!inProgress) throw new Error('composable did not start')
    await flushPromises()
    expect(inProgress.agentProgress['run-of-project-1']?.kind).toBe('ready')

    const pending: Array<() => void> = []
    vi.mocked(getProjectAgentTaskProgress).mockImplementation(((options: ProgressOptions) => new Promise((resolve) => {
      pending.push(() => resolve(ok(progressFor(options))))
    })) as never)
    const reload = inProgress.load()
    await flushPromises()
    expect(pending).toHaveLength(1)
    // No flash back to a skeleton: the last reading stays until the new one arrives.
    expect(inProgress.agentProgress['run-of-project-1']?.kind).toBe('ready')
    pending.forEach((settle) => settle())
    await reload
    expect(inProgress.agentProgress['run-of-project-1']?.kind).toBe('ready')

    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok({ items: [], nextCursor: null }))
    await inProgress.load()
    expect(inProgress.agentProgress).toEqual({})

    projectId.value = null
    await nextTick()
    expect(inProgress.agentProgress).toEqual({})
  })

  it('keeps at most four progress reads in flight across project switches, tab refreshes and row retries', async () => {
    vi.mocked(listProjectAgentRuns).mockImplementation((async (options: ListOptions) => ok({
      items: ['a', 'b', 'c'].map((suffix) => openRun(options.path.projectId, `run-${options.path.projectId}-${suffix}`)),
      nextCursor: null,
    })) as never)
    const { pending, stats } = controlProgressReads()
    const visibility = vi.spyOn(document, 'visibilityState', 'get')
    const projectId = ref<string | null>('project-1')
    scope = effectScope()
    const inProgress = scope.run(() => useProjectInProgress(projectId))
    if (!inProgress) throw new Error('composable did not start')
    await flushPromises()
    expect(stats.inFlight).toBe(3)

    // Switch while all three project-1 reads are still in flight.
    projectId.value = 'project-2'
    await nextTick()
    await flushPromises()
    expect(stats.inFlight).toBe(4)

    // A tab refresh queues another batch for project-2.
    visibility.mockReturnValue('hidden')
    document.dispatchEvent(new Event('visibilitychange'))
    visibility.mockReturnValue('visible')
    document.dispatchEvent(new Event('visibilitychange'))
    await flushPromises()
    expect(stats.inFlight).toBe(4)

    // Settle reads one by one; the first current project-2 read fails so its row can be retried mid-way.
    let retried = false
    while (pending.length) {
      const read = pending.shift()
      if (!read) break
      const failing = read.options.path.runId === 'run-project-2-a' && !retried
      read.settle(failing ? unavailable() : ok(progressFor(read.options)))
      await flushPromises()
      if (failing && inProgress.agentProgress['run-project-2-a']?.kind === 'error') {
        retried = true
        void inProgress.retryAgentProgress('run-project-2-a')
        await flushPromises()
      }
      expect(stats.inFlight).toBeLessThanOrEqual(4)
    }
    visibility.mockRestore()

    expect(retried).toBe(true)
    expect(stats.peak).toBe(4)
    expect(Object.keys(inProgress.agentProgress).sort()).toEqual(['run-project-2-a', 'run-project-2-b', 'run-project-2-c'])
    for (const state of Object.values(inProgress.agentProgress)) expect(phaseOf(state)).toBe('executing')
    // Superseded project-2 reads that were still waiting for a slot are never sent.
    const sent = vi.mocked(getProjectAgentTaskProgress).mock.calls.map(([options]) => (options as ProgressOptions).path.runId)
    expect(sent.filter((runId) => runId.startsWith('run-project-1-'))).toHaveLength(3)
    expect(sent.filter((runId) => runId === 'run-project-2-b')).toHaveLength(1)
  })

  it.each([
    ['an older success', (options: ProgressOptions) => ok(progressFor(options, 'queued'))],
    ['an older error', () => unavailable()],
  ])('keeps the latest read of a run when %s from an earlier refresh arrives after it', async (_label, olderResponse) => {
    vi.mocked(getProjectAgentTaskProgress).mockResolvedValue(unavailable())
    const projectId = ref<string | null>('project-1')
    scope = effectScope()
    const inProgress = scope.run(() => useProjectInProgress(projectId))
    if (!inProgress) throw new Error('composable did not start')
    await flushPromises()
    expect(inProgress.agentProgress['run-of-project-1']?.kind).toBe('error')

    const { pending } = controlProgressReads()
    const refresh = inProgress.load()
    await flushPromises()
    void inProgress.retryAgentProgress('run-of-project-1')
    await flushPromises()
    expect(pending).toHaveLength(2)
    const [batchRead, retryRead] = pending
    if (!batchRead || !retryRead) throw new Error('expected a refresh read and a retry read')

    retryRead.settle(ok(progressFor(retryRead.options, 'executing')))
    await flushPromises()
    expect(phaseOf(inProgress.agentProgress['run-of-project-1'])).toBe('executing')

    batchRead.settle(olderResponse(batchRead.options))
    await refresh
    await flushPromises()
    expect(phaseOf(inProgress.agentProgress['run-of-project-1'])).toBe('executing')
  })
})
