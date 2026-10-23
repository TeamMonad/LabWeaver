import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import ProjectAgentRunHistory from '@/components/common/ProjectAgentRunHistory.vue'
import { listProjectAgentRuns } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, listProjectAgentRuns: vi.fn() }
})

describe('ProjectAgentRunHistory', () => {
  beforeEach(() => {
    vi.resetAllMocks()
  })

  it('explains that the current page is filtered while more history is available', async () => {
    vi.mocked(listProjectAgentRuns).mockResolvedValue({
      data: {
        items: [{
          id: 'work-run-1',
          projectId: 'project-1',
          purpose: { kind: 'authoring', environmentClass: 'work' },
          state: 'failed',
          createdAt: '2026-07-11T00:00:00.000Z',
          updatedAt: '2026-07-11T00:01:00.000Z',
        }],
        nextCursor: 'cursor-next',
      } as never,
      error: undefined as never,
    })

    const wrapper = mount(ProjectAgentRunHistory, {
      props: { projectId: 'project-1', scope: 'experiment' },
    })
    await flushPromises()

    expect(wrapper.get('.history-empty').text()).toBe('本页没有可恢复的实验任务，可查看下一页。')
    expect(wrapper.get('.run-history-pagination button:last-child').text()).toBe('下一页')
    wrapper.unmount()
  })

  it('keeps a last empty page scoped to the page when an earlier page can contain tasks', async () => {
    vi.mocked(listProjectAgentRuns).mockResolvedValue({
      data: { items: [], nextCursor: null } as never,
      error: undefined as never,
    })

    const wrapper = mount(ProjectAgentRunHistory, {
      props: { projectId: 'project-1', scope: 'experiment' },
    })
    await flushPromises()

    expect(wrapper.get('.history-empty').text()).toBe('当前项目还没有可恢复的实验任务。')
    expect(wrapper.text()).not.toContain('本页没有可恢复的实验任务')
    wrapper.unmount()
  })

  it('refuses to show a history page that contains another project’s run', async () => {
    vi.mocked(listProjectAgentRuns).mockResolvedValue({
      data: {
        items: [{
          id: 'foreign-run',
          projectId: 'project-2',
          purpose: { kind: 'authoring', environmentClass: 'experiment' },
          state: 'awaiting_approval',
          createdAt: '2026-07-11T00:00:00.000Z',
          updatedAt: '2026-07-11T00:01:00.000Z',
        }],
        nextCursor: null,
      } as never,
      error: undefined as never,
    })

    const wrapper = mount(ProjectAgentRunHistory, {
      props: { projectId: 'project-1', scope: 'experiment' },
    })
    await flushPromises()

    expect(wrapper.text()).toContain('任务历史包含其他项目的记录，已停止显示')
    expect(wrapper.text()).not.toContain('foreign-run')
    expect(wrapper.find('.run-history-list').exists()).toBe(false)
    wrapper.unmount()
  })
})
