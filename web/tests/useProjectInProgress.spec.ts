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

function progressFor(options: ProgressOptions) {
  return {
    projectId: options.path.projectId, runId: options.path.runId, page: 1, pageSize: 1, hasMore: false, revision: 1, state: 'running',
    current: [{ track: 'work_configuration', progress: { attempt: 1, phase: 'executing', nextSteps: [] } }],
    history: [],
  }
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
})
