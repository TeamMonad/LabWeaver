import { flushPromises } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { effectScope, nextTick, ref } from 'vue'
import { getProjectAvailableActions } from '@/generated/contracts'
import { useProjectAvailableActions } from '@/composables/useProjectAvailableActions'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getProjectAvailableActions: vi.fn(),
  }
})

function answer(projectId: string, actions: string[], validUntil = '2099-01-01T00:00:00.000Z') {
  return { data: { projectId, actorId: 'actor-1', membershipRevision: 3, validUntil, actions }, error: undefined } as never
}

function mountActions(projectId: string | null = 'project-1') {
  const scope = effectScope()
  const id = ref<string | null>(projectId)
  const actions = scope.run(() => useProjectAvailableActions(id))!
  return { scope, id, actions }
}

describe('useProjectAvailableActions', () => {
  beforeEach(() => {
    vi.mocked(getProjectAvailableActions).mockReset()
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('offers only the returned actions and offers nothing while the answer is loading', async () => {
    vi.mocked(getProjectAvailableActions).mockResolvedValue(answer('project-1', ['browse', 'start', 'stop']))
    const { scope, actions } = mountActions()
    expect(actions.allows('start')).toBe(false)
    expect(actions.denialReason('start')).not.toBeNull()
    await flushPromises()
    expect(getProjectAvailableActions).toHaveBeenCalledWith({ path: { projectId: 'project-1' } })
    expect(actions.allows('start')).toBe(true)
    expect(actions.allows('stop')).toBe(true)
    expect(actions.allows('edit')).toBe(false)
    expect(actions.denialReason('start')).toBeNull()
    // Every action that is not offered has a reason; the purpose names the user's task.
    expect(actions.denialReason('manage_members')).not.toBeNull()
    expect(actions.denialReason('edit', '提交评测')).toContain('提交评测')
    scope.stop()
  })

  it('fails closed with the server diagnostic when the read is refused', async () => {
    vi.mocked(getProjectAvailableActions).mockResolvedValue({
      data: undefined,
      error: { type: 'about:blank', title: 'Forbidden', status: 403, detail: '当前账号不是该项目成员。', diagnosticCode: 'LW_AUTH_SCOPE_DENIED', retryable: false, requestId: 'req-403' },
    } as never)
    const { scope, actions } = mountActions()
    await flushPromises()
    expect(actions.allows('browse')).toBe(false)
    expect(actions.state.value).toMatchObject({ kind: 'error', diagnostic: { code: 'LW_AUTH_SCOPE_DENIED', requestId: 'req-403', status: 403 } })
    // The server's reason is what the user reads, not a generic message.
    expect(actions.denialReason('start')).toContain('当前账号不是该项目成员。')
    scope.stop()
  })

  it('rejects an answer for another project or with an unknown action', async () => {
    vi.mocked(getProjectAvailableActions).mockResolvedValueOnce(answer('project-2', ['browse', 'edit']))
    const { scope, actions } = mountActions()
    await flushPromises()
    expect(actions.allows('edit')).toBe(false)
    expect(actions.state.value).toMatchObject({ kind: 'error', diagnostic: { code: 'PROJECT_AVAILABLE_ACTIONS_INVALID' } })

    vi.mocked(getProjectAvailableActions).mockResolvedValueOnce(answer('project-1', ['browse', 'superuser']))
    await actions.load()
    expect(actions.allows('browse')).toBe(false)
    scope.stop()
  })

  it('drops a slower answer for the previous project after the project changes', async () => {
    let resolveFirst: (value: never) => void = () => undefined
    vi.mocked(getProjectAvailableActions)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveFirst = resolve as never }))
      .mockResolvedValueOnce(answer('project-2', ['browse']))
    const { scope, id, actions } = mountActions()
    id.value = 'project-2'
    await nextTick()
    await flushPromises()
    resolveFirst(answer('project-1', ['browse', 'edit', 'manage_members']))
    await flushPromises()
    expect(actions.current.value?.projectId).toBe('project-2')
    expect(actions.allows('edit')).toBe(false)
    scope.stop()
  })

  it('stops offering actions once validUntil passes and reads them again', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-10-23T08:00:00.000Z'))
    vi.mocked(getProjectAvailableActions)
      .mockResolvedValueOnce(answer('project-1', ['browse', 'start'], '2026-10-23T08:00:10.000Z'))
      .mockResolvedValueOnce(answer('project-1', ['browse'], '2026-10-23T09:00:00.000Z'))
    const { scope, actions } = mountActions()
    await flushPromises()
    expect(actions.allows('start')).toBe(true)
    await vi.advanceTimersByTimeAsync(11_000)
    await flushPromises()
    // The member was demoted meanwhile: the new answer, not the expired one, decides.
    expect(actions.allows('start')).toBe(false)
    expect(actions.denialReason('start')).not.toBeNull()
    scope.stop()
  })

  it('refresh keeps the current answer on screen until the new one arrives', async () => {
    let resolveSecond: (value: never) => void = () => undefined
    vi.mocked(getProjectAvailableActions)
      .mockResolvedValueOnce(answer('project-1', ['browse', 'edit']))
      .mockImplementationOnce(() => new Promise((resolve) => { resolveSecond = resolve as never }))
    const { scope, actions } = mountActions()
    await flushPromises()
    const refreshing = actions.refresh()
    expect(actions.allows('edit')).toBe(true)
    resolveSecond(answer('project-1', ['browse']))
    await refreshing
    expect(actions.allows('edit')).toBe(false)
    scope.stop()
  })

  it('offers nothing without a project', async () => {
    const { scope, actions } = mountActions(null)
    await flushPromises()
    expect(getProjectAvailableActions).not.toHaveBeenCalled()
    expect(actions.allows('edit')).toBe(false)
    expect(actions.denialReason('edit')).not.toBeNull()
    scope.stop()
  })
})
