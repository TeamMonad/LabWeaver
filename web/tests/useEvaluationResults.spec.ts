import { effectScope, ref } from 'vue'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useEvaluationResult, useEvaluationResults } from '@/composables/useEvaluationResults'
import { getOwnProjectEvaluationResult, listOwnProjectEvaluationResults } from '@/generated/contracts'

const mocks = vi.hoisted(() => ({
  getOwnProjectEvaluationResult: vi.fn(),
  listOwnProjectEvaluationResults: vi.fn(),
}))

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, ...mocks }
})

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise
  })
  return { promise, resolve }
}

function result(projectId: string, runId: string) {
  return {
    runId,
    projectId,
    courseId: null,
    releaseId: `release-${runId}`,
    frozenSubmissionId: `submission-${runId}`,
    revision: 1,
    state: 'succeeded',
    awardedScore: 10,
    maxScore: 10,
    createdAt: '2026-09-14T10:00:00.000Z',
    updatedAt: '2026-09-14T10:01:00.000Z',
    completedAt: '2026-09-14T10:01:00.000Z',
    steps: [],
  } as never
}

function listResponse(items: unknown[], nextCursor?: string | null) {
  return {
    data: { items, ...(nextCursor !== undefined ? { nextCursor } : {}) },
    error: undefined as never,
  } as never
}

function withList(projectId: ReturnType<typeof ref<string | undefined>>) {
  const scope = effectScope()
  let state!: ReturnType<typeof useEvaluationResults>
  scope.run(() => {
    state = useEvaluationResults(projectId)
  })
  return { state, stop: () => scope.stop() }
}

function withDetail(
  projectId: ReturnType<typeof ref<string | undefined>>,
  runId: ReturnType<typeof ref<string | undefined>>,
) {
  const scope = effectScope()
  let state!: ReturnType<typeof useEvaluationResult>
  scope.run(() => {
    state = useEvaluationResult(projectId, runId)
  })
  return { state, stop: () => scope.stop() }
}

describe('useEvaluationResults', () => {
  beforeEach(() => {
    vi.resetAllMocks()
  })

  it('does not let a slow project response overwrite the newly selected project', async () => {
    const first = deferred<unknown>()
    const second = deferred<unknown>()
    vi.mocked(listOwnProjectEvaluationResults).mockImplementation(({ path }) => {
      return path.projectId === 'project-a' ? first.promise as never : second.promise as never
    })

    const projectId = ref<string | undefined>('project-a')
    const { state, stop } = withList(projectId)
    await vi.waitFor(() => expect(listOwnProjectEvaluationResults).toHaveBeenCalledWith({
      path: { projectId: 'project-a' },
      query: { cursor: undefined, limit: 50 },
    }))

    projectId.value = 'project-b'
    await vi.waitFor(() => expect(listOwnProjectEvaluationResults).toHaveBeenCalledWith({
      path: { projectId: 'project-b' },
      query: { cursor: undefined, limit: 50 },
    }))
    second.resolve(listResponse([result('project-b', 'run-b')]))
    await vi.waitFor(() => expect(state.results).toEqual({
      kind: 'success',
      data: [result('project-b', 'run-b')],
    }))

    first.resolve(listResponse([result('project-a', 'run-a')]))
    await Promise.resolve()
    expect(state.results).toEqual({ kind: 'success', data: [result('project-b', 'run-b')] })
    stop()
  })

  it('does not append an old pagination response to the newly selected project', async () => {
    const oldPage = deferred<unknown>()
    vi.mocked(listOwnProjectEvaluationResults).mockImplementation(({ path, query }) => {
      if (path.projectId === 'project-a' && query?.cursor === undefined) {
        return Promise.resolve(listResponse([result('project-a', 'run-a')], 'a-page-2')) as never
      }
      if (path.projectId === 'project-a') return oldPage.promise as never
      return Promise.resolve(listResponse([result('project-b', 'run-b')])) as never
    })

    const projectId = ref<string | undefined>('project-a')
    const { state, stop } = withList(projectId)
    await vi.waitFor(() => expect(state.results).toEqual({
      kind: 'success',
      data: [result('project-a', 'run-a')],
    }))
    const pagination = state.loadMore()
    await vi.waitFor(() => expect(state.loadingMore).toBe(true))

    projectId.value = 'project-b'
    await vi.waitFor(() => expect(state.results).toEqual({
      kind: 'success',
      data: [result('project-b', 'run-b')],
    }))
    expect(state.nextCursor).toBeNull()
    expect(state.loadingMore).toBe(false)
    expect(state.loadMoreError).toBeNull()

    oldPage.resolve(listResponse([result('project-a', 'run-a-2')]))
    await pagination
    expect(state.results).toEqual({ kind: 'success', data: [result('project-b', 'run-b')] })
    stop()
  })

  it('rejects duplicate pagination requests and does not duplicate rows', async () => {
    const page = deferred<unknown>()
    vi.mocked(listOwnProjectEvaluationResults).mockImplementation(({ query }) => {
      if (query?.cursor === undefined) return Promise.resolve(listResponse([result('project-a', 'run-a')], 'a-page-2')) as never
      return page.promise as never
    })

    const projectId = ref<string | undefined>('project-a')
    const { state, stop } = withList(projectId)
    await vi.waitFor(() => expect(state.results.kind).toBe('success'))
    const firstPageLoad = state.loadMore()
    const duplicatePageLoad = state.loadMore()
    await vi.waitFor(() => expect(listOwnProjectEvaluationResults).toHaveBeenCalledTimes(2))

    page.resolve(listResponse([result('project-a', 'run-a-2')]))
    await Promise.all([firstPageLoad, duplicatePageLoad])
    expect(state.results).toEqual({
      kind: 'success',
      data: [result('project-a', 'run-a'), result('project-a', 'run-a-2')],
    })

    await state.loadMore()
    expect(listOwnProjectEvaluationResults).toHaveBeenCalledTimes(2)
    stop()
  })

  it('surfaces a thrown list request as an error state', async () => {
    vi.mocked(listOwnProjectEvaluationResults).mockRejectedValue(new Error('evaluation service unavailable'))
    const projectId = ref<string | undefined>('project-a')
    const { state, stop } = withList(projectId)

    await vi.waitFor(() => expect(state.results).toEqual({
      kind: 'error',
      diagnostic: {
        code: 'EVALUATION_RESULTS_LOAD_FAILED',
        message: '加载评测结果失败',
        retryable: true,
      },
    }))
    stop()
  })

  it('does not let a slow detail response for an old run overwrite the current run', async () => {
    const first = deferred<unknown>()
    const second = deferred<unknown>()
    vi.mocked(getOwnProjectEvaluationResult).mockImplementation(({ path }) => {
      return path.runId === 'run-a' ? first.promise as never : second.promise as never
    })

    const projectId = ref<string | undefined>('project-a')
    const runId = ref<string | undefined>('run-a')
    const { state, stop } = withDetail(projectId, runId)
    await vi.waitFor(() => expect(getOwnProjectEvaluationResult).toHaveBeenCalledWith({
      path: { projectId: 'project-a', runId: 'run-a' },
    }))

    runId.value = 'run-b'
    await vi.waitFor(() => expect(getOwnProjectEvaluationResult).toHaveBeenCalledWith({
      path: { projectId: 'project-a', runId: 'run-b' },
    }))
    second.resolve({ data: result('project-a', 'run-b'), error: undefined as never })
    await vi.waitFor(() => expect(state.result).toEqual({
      kind: 'success',
      data: result('project-a', 'run-b'),
    }))

    first.resolve({ data: result('project-a', 'run-a'), error: undefined as never })
    await Promise.resolve()
    expect(state.result).toEqual({ kind: 'success', data: result('project-a', 'run-b') })
    stop()
  })

  it('surfaces a thrown detail request as an error state', async () => {
    vi.mocked(getOwnProjectEvaluationResult).mockRejectedValue(new Error('evaluation service unavailable'))
    const projectId = ref<string | undefined>('project-a')
    const runId = ref<string | undefined>('run-a')
    const { state, stop } = withDetail(projectId, runId)

    await vi.waitFor(() => expect(state.result).toEqual({
      kind: 'error',
      diagnostic: {
        code: 'EVALUATION_RESULT_LOAD_FAILED',
        message: '加载评测详情失败',
        retryable: true,
      },
    }))
    stop()
  })
})
