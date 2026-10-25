import { effectScope, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { useProjectResources } from '@/composables/useProjectResources'

const mocks = vi.hoisted(() => ({
  cancelResourceRequest: vi.fn(),
  createResourceRequest: vi.fn(),
  getResourceLease: vi.fn(),
  getResourceRequest: vi.fn(),
  listProjectResourceLeases: vi.fn(),
  listProjectResourceRequests: vi.fn(),
  renewResourceLease: vi.fn(),
  revokeResourceLease: vi.fn(),
}))

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, ...mocks }
})

function success<T>(data: T) {
  return { data, error: undefined }
}
function page(items: unknown[], nextCursor: string | null = null) {
  return success({ items, nextCursor })
}

function request(id: string, state: 'reviewing' | 'active' = 'reviewing') {
  return { id, requestKey: `request-${id}`, state } as never
}

function lease(id: string, state: 'allocating' | 'active' = 'active') {
  return { id, requestId: `request-${id}`, state } as never
}

afterEach(() => {
  vi.useRealTimers()
  Object.values(mocks).forEach((mock) => mock.mockReset())
})

describe('useProjectResources', () => {
  it('loads requests and leases when the project context becomes available', async () => {
    mocks.listProjectResourceRequests.mockResolvedValue(page([request('one')]))
    mocks.listProjectResourceLeases.mockResolvedValue(page([lease('one')]))

    const projectId = ref<string | null>('project-1')
    const scope = effectScope()
    let state!: ReturnType<typeof useProjectResources>
    scope.run(() => { state = useProjectResources(projectId) })

    await vi.waitFor(() => expect(state.requests).toEqual({ kind: 'success', data: [request('one')] }))
    expect(state.leases).toEqual({ kind: 'success', data: [lease('one')] })
    expect(mocks.listProjectResourceRequests).toHaveBeenCalledWith({ path: { projectId: 'project-1' }, query: { limit: 100 } })
    expect(mocks.listProjectResourceLeases).toHaveBeenCalledWith({ path: { projectId: 'project-1' }, query: { limit: 100 } })
    scope.stop()
  })

  it('follows lease pages to the end before deciding which requests are allocated', async () => {
    mocks.listProjectResourceRequests.mockResolvedValue(page([request('one'), request('two')]))
    mocks.listProjectResourceLeases
      .mockResolvedValueOnce(page([lease('one')], 'lease-2'))
      .mockResolvedValueOnce(page([lease('two')]))

    const projectId = ref<string | null>('project-1')
    const scope = effectScope()
    const state = scope.run(() => useProjectResources(projectId))
    if (!state) throw new Error('useProjectResources did not run in the effect scope')

    await vi.waitFor(() => expect(state.leases).toEqual({ kind: 'success', data: [lease('one'), lease('two')] }))
    expect(mocks.listProjectResourceLeases.mock.calls).toEqual([
      [{ path: { projectId: 'project-1' }, query: { limit: 100 } }],
      [{ path: { projectId: 'project-1' }, query: { limit: 100, cursor: 'lease-2' } }],
    ])
    scope.stop()
  })

  it('surfaces rejected list requests as readable diagnostics', async () => {
    mocks.listProjectResourceRequests.mockRejectedValue(new Error('resource API unavailable'))
    mocks.listProjectResourceLeases.mockResolvedValue(page([]))

    const projectId = ref<string | null>('project-1')
    const scope = effectScope()
    let state!: ReturnType<typeof useProjectResources>
    scope.run(() => { state = useProjectResources(projectId) })

    await vi.waitFor(() => expect(state.requests).toEqual({
      kind: 'error',
      diagnostic: {
        code: 'PROJECT_RESOURCE_REQUESTS_LOAD_FAILED',
        message: '加载项目资源申请失败',
        retryable: true,
      },
    }))
    expect(state.leases).toEqual({ kind: 'empty' })
    scope.stop()
  })

  it('does not let an in-flight poll overwrite a newly selected project', async () => {
    vi.useFakeTimers()
    let resolvePollRequests!: (value: unknown) => void
    let resolvePollLeases!: (value: unknown) => void
    const pollRequests = new Promise((resolve) => { resolvePollRequests = resolve })
    const pollLeases = new Promise((resolve) => { resolvePollLeases = resolve })
    mocks.listProjectResourceRequests
      .mockResolvedValueOnce(page([request('one', 'active')]))
      .mockImplementation(({ path }: { path: { projectId: string } }) => (
        path.projectId === 'project-1' ? pollRequests : Promise.resolve(page([]))
      ))
    mocks.listProjectResourceLeases
      .mockResolvedValueOnce(page([lease('one', 'active')]))
      .mockImplementation(({ path }: { path: { projectId: string } }) => (
        path.projectId === 'project-1' ? pollLeases : Promise.resolve(page([]))
      ))

    const projectId = ref<string | null>('project-1')
    const scope = effectScope()
    let state!: ReturnType<typeof useProjectResources>
    scope.run(() => { state = useProjectResources(projectId) })
    await vi.waitFor(() => expect(state.requests).toEqual({ kind: 'success', data: [request('one', 'active')] }))

    await vi.advanceTimersByTimeAsync(5000)
    expect(mocks.listProjectResourceRequests).toHaveBeenCalledTimes(2)
    expect(mocks.listProjectResourceLeases).toHaveBeenCalledTimes(2)

    projectId.value = 'project-2'
    await vi.waitFor(() => expect(state.requests).toEqual({ kind: 'empty' }))
    expect(state.leases).toEqual({ kind: 'empty' })

    resolvePollRequests(page([request('stale', 'active')]))
    resolvePollLeases(page([lease('stale', 'active')]))
    await Promise.resolve()
    await Promise.resolve()
    expect(state.requests).toEqual({ kind: 'empty' })
    expect(state.leases).toEqual({ kind: 'empty' })
    scope.stop()
  })
})
