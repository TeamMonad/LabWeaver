import { flushPromises } from '@vue/test-utils'
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { effectScope, ref } from 'vue'
import { getProjectAvailableActions } from '@/generated/contracts'
import { useProjectAvailableActions } from '@/composables/useProjectAvailableActions'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getProjectAvailableActions: vi.fn(),
  }
})

describe('useProjectAvailableActions', () => {
  beforeEach(() => {
    vi.mocked(getProjectAvailableActions).mockReset()
  })

  it('exposes start/stop from available-actions and fails closed while loading', async () => {
    vi.mocked(getProjectAvailableActions).mockResolvedValue({
      data: {
        projectId: 'project-1',
        actorId: 'actor-1',
        membershipRevision: 3,
        validUntil: '2026-10-12T12:00:00.000Z',
        actions: ['browse', 'start', 'stop'],
      },
      error: undefined,
    } as never)

    const scope = effectScope()
    const projectId = ref<string | null>('project-1')
    const actions = scope.run(() => useProjectAvailableActions(projectId))!
    expect(actions.canStart.value).toBe(false)
    await flushPromises()
    expect(getProjectAvailableActions).toHaveBeenCalledWith({ path: { projectId: 'project-1' } })
    expect(actions.canStart.value).toBe(true)
    expect(actions.canStop.value).toBe(true)
    expect(actions.canEdit.value).toBe(false)
    expect(actions.allows('manage_members')).toBe(false)
    scope.stop()
  })
})
