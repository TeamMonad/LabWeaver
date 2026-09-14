import { effectScope, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { useProjectMemberships } from '@/composables/useProjects'

const listProjectMemberships = vi.hoisted(() => vi.fn())

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, listProjectMemberships }
})

function result(data: unknown[]) {
  return { data, error: undefined as never }
}

function membership(actorId: string) {
  return { actorId, role: 'student', state: 'active', revision: 1 } as never
}

afterEach(() => {
  listProjectMemberships.mockReset()
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
