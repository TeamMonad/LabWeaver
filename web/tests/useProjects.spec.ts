import { effectScope, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { useOrganizationDirectoryUsers, useProjectMemberships } from '@/composables/useProjects'

const listProjectMemberships = vi.hoisted(() => vi.fn())
const listOrganizationUsers = vi.hoisted(() => vi.fn())

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, listProjectMemberships, listOrganizationUsers }
})

function result(data: unknown[]) {
  return { data, error: undefined as never }
}

function membership(actorId: string) {
  return { actorId, role: 'student', state: 'active', revision: 1 } as never
}

afterEach(() => {
  listProjectMemberships.mockReset()
  listOrganizationUsers.mockReset()
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
    expect(listProjectMemberships).toHaveBeenCalledWith({ path: { projectId: 'project-1' } })
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
