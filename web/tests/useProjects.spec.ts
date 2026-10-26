import { defineComponent, effectScope, ref } from 'vue'
import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { useOrganizationDirectoryUsers, useProjectMemberships, useProjects } from '@/composables/useProjects'
import { resetProjectCatalog } from './projectCatalog'

const listProjectMemberships = vi.hoisted(() => vi.fn())
const listOrganizationUsers = vi.hoisted(() => vi.fn())
const listProjects = vi.hoisted(() => vi.fn())

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, listProjectMemberships, listOrganizationUsers, listProjects }
})

function result(items: unknown[], nextCursor: string | null = null) {
  return { data: { items, nextCursor }, error: undefined as never }
}

function membership(actorId: string) {
  return { actorId, role: 'student', state: 'active', revision: 1 } as never
}

afterEach(() => {
  listProjectMemberships.mockReset()
  listOrganizationUsers.mockReset()
  listProjects.mockReset()
})

describe('useProjects shared catalog', () => {
  const Consumer = defineComponent({
    setup() {
      useProjects()
      return () => null
    },
  })
  const project = { id: 'project-1', name: '项目', state: 'active', revision: 1, ownerActorId: 'actor-1', createdAt: '', updatedAt: '' }

  it('reads an unread catalog once and does not reload it when more consumers mount', async () => {
    resetProjectCatalog()
    listProjects.mockResolvedValue(result([project]))
    const first = mount(Consumer)
    // A second consumer mounting while the first read is in flight joins it.
    const second = mount(Consumer)
    await flushPromises()
    expect(listProjects).toHaveBeenCalledTimes(1)
    expect(useProjects().projects).toEqual({ kind: 'success', data: [project] })

    // A consumer mounting on a loaded catalog keeps it loaded instead of returning it to `loading`.
    const third = mount(Consumer)
    expect(useProjects().projects.kind).toBe('success')
    await flushPromises()
    expect(listProjects).toHaveBeenCalledTimes(1)

    // An explicit refresh still reads the list again.
    await useProjects().load()
    expect(listProjects).toHaveBeenCalledTimes(2)
    for (const wrapper of [first, second, third]) wrapper.unmount()
  })

  it('reads the catalog again when a consumer mounts after a failed read', async () => {
    resetProjectCatalog()
    listProjects.mockResolvedValueOnce({ data: undefined, error: { status: 503, diagnosticCode: 'LW_HTTP_SERVICE_UNAVAILABLE', detail: '项目服务暂时不可用', retryable: true } })
    const first = mount(Consumer)
    await flushPromises()
    expect(useProjects().projects.kind).toBe('error')

    listProjects.mockResolvedValueOnce(result([project]))
    const second = mount(Consumer)
    await flushPromises()
    expect(listProjects).toHaveBeenCalledTimes(2)
    expect(useProjects().projects).toEqual({ kind: 'success', data: [project] })
    first.unmount()
    second.unmount()
  })
})

describe('useProjectMemberships', () => {
  it('loads members when the composable is mounted', async () => {
    listProjectMemberships.mockResolvedValueOnce(result([membership('student-1')]))
    const projectId = ref<string | null>('project-1')
    const scope = effectScope()
    let state!: ReturnType<typeof useProjectMemberships>
    scope.run(() => { state = useProjectMemberships(projectId) })

    await vi.waitFor(() => expect(state.memberships).toEqual({
      kind: 'success',
      data: [membership('student-1')],
    }))
    expect(listProjectMemberships).toHaveBeenCalledWith({ path: { projectId: 'project-1' }, query: { limit: 50 } })
    expect(state.hasMore).toBe(false)
    scope.stop()
  })

  it('clears the previous members and outcome while switching projects', async () => {
    let resolveFirst!: (value: unknown) => void
    let resolveSecond!: (value: unknown) => void
    const first = new Promise((resolve) => { resolveFirst = resolve })
    const second = new Promise((resolve) => { resolveSecond = resolve })
    listProjectMemberships
      .mockImplementationOnce(() => first)
      .mockImplementationOnce(() => second)

    const projectId = ref<string | null>('project-1')
    const scope = effectScope()
    let state!: ReturnType<typeof useProjectMemberships>
    scope.run(() => { state = useProjectMemberships(projectId) })
    await vi.waitFor(() => expect(listProjectMemberships).toHaveBeenCalledTimes(1))
    resolveFirst(result([membership('student-1')]))
    await vi.waitFor(() => expect(state.memberships.kind).toBe('success'))

    state.outcome = { kind: 'success', diagnostic: { code: 'TEST', message: '已更新', retryable: false } }
    projectId.value = 'project-2'
    await vi.waitFor(() => expect(listProjectMemberships).toHaveBeenCalledTimes(2))
    expect(state.memberships).toEqual({ kind: 'loading', message: '加载项目成员…' })
    expect(state.outcome).toBeNull()

    resolveSecond(result([membership('student-2')]))
    await vi.waitFor(() => expect(state.memberships).toEqual({
      kind: 'success',
      data: [membership('student-2')],
    }))
    scope.stop()
  })

  it('does not let a late response from the previous project overwrite members', async () => {
    let resolveFirst!: (value: unknown) => void
    let resolveSecond!: (value: unknown) => void
    const first = new Promise((resolve) => { resolveFirst = resolve })
    const second = new Promise((resolve) => { resolveSecond = resolve })
    listProjectMemberships
      .mockImplementationOnce(() => first)
      .mockImplementationOnce(() => second)

    const projectId = ref<string | null>('project-1')
    const scope = effectScope()
    let state!: ReturnType<typeof useProjectMemberships>
    scope.run(() => { state = useProjectMemberships(projectId) })
    await vi.waitFor(() => expect(listProjectMemberships).toHaveBeenCalledTimes(1))

    projectId.value = 'project-2'
    await vi.waitFor(() => expect(listProjectMemberships).toHaveBeenCalledTimes(2))
    resolveSecond(result([membership('student-2')]))
    await vi.waitFor(() => expect(state.memberships).toEqual({
      kind: 'success',
      data: [membership('student-2')],
    }))

    resolveFirst(result([membership('student-1')]))
    await Promise.resolve()
    expect(state.memberships).toEqual({
      kind: 'success',
      data: [membership('student-2')],
    })
    scope.stop()
  })

  it('appends the next cursor page and stops at the last page', async () => {
    listProjectMemberships
      .mockResolvedValueOnce(result([membership('student-1')], 'cursor-2'))
      .mockResolvedValueOnce(result([membership('student-2')]))
    const projectId = ref<string | null>('project-1')
    const scope = effectScope()
    let state!: ReturnType<typeof useProjectMemberships>
    scope.run(() => { state = useProjectMemberships(projectId) })
    await vi.waitFor(() => expect(state.hasMore).toBe(true))

    await state.loadMore()
    expect(listProjectMemberships).toHaveBeenLastCalledWith({ path: { projectId: 'project-1' }, query: { limit: 50, cursor: 'cursor-2' } })
    expect(state.memberships).toEqual({ kind: 'success', data: [membership('student-1'), membership('student-2')] })
    expect(state.hasMore).toBe(false)

    await state.loadMore()
    expect(listProjectMemberships).toHaveBeenCalledTimes(2)
    scope.stop()
  })

  it.each([false, 0, undefined, ''])('rejects invalid member page continuation %j instead of presenting a complete list', async (nextCursor) => {
    listProjectMemberships.mockResolvedValueOnce({ data: { items: [membership('student-partial')], nextCursor } })
    const scope = effectScope()
    const state = scope.run(() => useProjectMemberships(ref('project-1')))
    if (!state) throw new Error('项目成员查询未进入测试作用域')

    await vi.waitFor(() => expect(state.memberships).toMatchObject({ kind: 'error', diagnostic: { code: 'PROJECT_MEMBERS_LIST_FAILED' } }))
    expect(state.memberships).not.toHaveProperty('data')
    expect(state.hasMore).toBe(false)
    scope.stop()
  })

  it('keeps the valid member prefix on a repeated cursor and retries the rejected page', async () => {
    listProjectMemberships
      .mockResolvedValueOnce(result([membership('student-1')], 'cursor-2'))
      .mockResolvedValueOnce(result([membership('student-invalid')], 'cursor-2'))
      .mockResolvedValueOnce(result([membership('student-2')]))
    const scope = effectScope()
    const state = scope.run(() => useProjectMemberships(ref('project-1')))
    if (!state) throw new Error('项目成员查询未进入测试作用域')
    await vi.waitFor(() => expect(state.hasMore).toBe(true))

    await state.loadMore()
    expect(state.memberships).toEqual({ kind: 'success', data: [membership('student-1')] })
    expect(state.loadMoreError).toMatchObject({ code: 'PROJECT_MEMBERS_LIST_MORE_FAILED' })
    expect(state.hasMore).toBe(true)
    await state.loadMore()
    expect(state.memberships).toEqual({ kind: 'success', data: [membership('student-1'), membership('student-2')] })
    expect(state.loadMoreError).toBeNull()
    expect(state.hasMore).toBe(false)
    scope.stop()
  })

  it('keeps loaded members when a later page fails and drops a page that arrives after a project switch', async () => {
    let resolveLate!: (value: unknown) => void
    listProjectMemberships
      .mockResolvedValueOnce(result([membership('student-1')], 'cursor-2'))
      .mockResolvedValueOnce({ data: undefined, error: { diagnosticCode: 'LW_HTTP_SERVICE_UNAVAILABLE', detail: '成员服务暂时不可用', retryable: true } })
      .mockImplementationOnce(() => new Promise((resolve) => { resolveLate = resolve }))
      .mockResolvedValueOnce(result([membership('student-9')]))
    const projectId = ref<string | null>('project-1')
    const scope = effectScope()
    let state!: ReturnType<typeof useProjectMemberships>
    scope.run(() => { state = useProjectMemberships(projectId) })
    await vi.waitFor(() => expect(state.hasMore).toBe(true))

    await state.loadMore()
    expect(state.loadMoreError).toMatchObject({ code: 'LW_HTTP_SERVICE_UNAVAILABLE', message: '成员服务暂时不可用' })
    expect(state.memberships).toEqual({ kind: 'success', data: [membership('student-1')] })
    expect(state.hasMore).toBe(true)

    const late = state.loadMore()
    projectId.value = 'project-2'
    await vi.waitFor(() => expect(state.memberships).toEqual({ kind: 'success', data: [membership('student-9')] }))
    resolveLate(result([membership('student-2')]))
    await late
    expect(state.memberships).toEqual({ kind: 'success', data: [membership('student-9')] })
    expect(state.loadMoreError).toBeNull()
    scope.stop()
  })
})

describe('useOrganizationDirectoryUsers', () => {
  it('loads a bounded server page for the entered organization query', async () => {
    const page = { items: [{ username: 'student-1', displayName: '学生一', enabled: true }], page: 1, pageSize: 25, hasMore: true }
    listOrganizationUsers.mockResolvedValueOnce({ data: page, error: undefined })
    const scope = effectScope()
    let state!: ReturnType<typeof useOrganizationDirectoryUsers>
    scope.run(() => { state = useOrganizationDirectoryUsers() })

    await state.load('student', 1)
    expect(listOrganizationUsers).toHaveBeenCalledWith({ query: { query: 'student', page: 1, pageSize: 25 } })
    expect(state.users).toEqual({ kind: 'success', data: page })
    scope.stop()
  })

  it('explains directory unavailability while preserving the diagnostic and retry behavior', async () => {
    const page = { items: [{ username: 'student-1', displayName: '学生一', enabled: true }], page: 1, pageSize: 25, hasMore: false }
    listOrganizationUsers
      .mockResolvedValueOnce({
        data: undefined as never,
        error: { diagnosticCode: 'LW_ACCESS_DIRECTORY_UNAVAILABLE', detail: 'directory offline', retryable: true } as never,
      })
      .mockResolvedValueOnce({ data: page, error: undefined as never })
    const scope = effectScope()
    let state!: ReturnType<typeof useOrganizationDirectoryUsers>
    scope.run(() => { state = useOrganizationDirectoryUsers() })

    await state.load('student', 1)
    expect(state.users).toEqual({
      kind: 'error',
      diagnostic: {
        code: 'LW_ACCESS_DIRECTORY_UNAVAILABLE',
        message: '平台账号目录暂时不可用，请稍后重试。已创建的项目和材料会保留。',
        retryable: true,
      },
    })

    await state.load('student', 1)
    expect(state.users).toEqual({ kind: 'success', data: page })
    scope.stop()
  })

  it('ignores a late result after a new query or clear', async () => {
    let resolveFirst!: (value: unknown) => void
    let resolveSecond!: (value: unknown) => void
    listOrganizationUsers
      .mockImplementationOnce(() => new Promise((resolve) => { resolveFirst = resolve }))
      .mockImplementationOnce(() => new Promise((resolve) => { resolveSecond = resolve }))

    const scope = effectScope()
    let state!: ReturnType<typeof useOrganizationDirectoryUsers>
    scope.run(() => { state = useOrganizationDirectoryUsers() })
    const first = state.load('old', 1)
    const second = state.load('new', 1)
    resolveSecond({ data: { items: [], page: 1, pageSize: 25, hasMore: false }, error: undefined })
    await second
    expect(state.users).toEqual({ kind: 'success', data: { items: [], page: 1, pageSize: 25, hasMore: false } })

    state.clear()
    resolveFirst({ data: { items: [{ username: 'old', displayName: '旧账号', enabled: true }], page: 1, pageSize: 25, hasMore: false }, error: undefined })
    await first
    expect(state.users).toEqual({ kind: 'idle' })
    scope.stop()
  })

  it('does not query for an empty or overlong search', async () => {
    const scope = effectScope()
    let state!: ReturnType<typeof useOrganizationDirectoryUsers>
    scope.run(() => { state = useOrganizationDirectoryUsers() })

    await state.load('')
    expect(listOrganizationUsers).not.toHaveBeenCalled()
    await state.load('x'.repeat(129))
    expect(listOrganizationUsers).not.toHaveBeenCalled()
    expect(state.users).toMatchObject({ kind: 'error', diagnostic: { code: 'DIRECTORY_QUERY_TOO_LONG' } })
    scope.stop()
  })
})
