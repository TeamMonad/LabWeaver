import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import ResourceFinanceView from '@/views/admin/ResourceFinanceView.vue'

const api = vi.hoisted(() => ({
  get: vi.fn(),
  put: vi.fn(),
  post: vi.fn(),
}))

vi.mock('@/api/client', () => ({ apiClient: api }))

vi.mock('@/composables/useProjects', () => ({
  useProjects: () => ({
    projects: {
      kind: 'success',
      data: [{ id: 'project-new', name: '新项目', courseId: null }],
    },
  }),
}))

describe('ResourceFinanceView', () => {
  beforeEach(() => {
    api.get.mockReset()
  })

  it('shows the editable create form when Resource reports a missing project budget', async () => {
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) {
        // This is the generated Axios transport shape for Resource's plain
        // text 404 diagnostic body.
        return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      }
      return Promise.resolve({ data: [] })
    })

    const wrapper = mount(ResourceFinanceView)
    await flushPromises()

    expect(wrapper.find('.budget-form').exists()).toBe(true)
    expect(wrapper.find('.budget-form button[type="submit"]').text()).toBe('创建预算')
    expect(wrapper.find('.budget-card .diagnostic-banner').exists()).toBe(false)
  })

  it('keeps an unrelated 404 as a budget load error', async () => {
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) {
        return Promise.resolve({ error: 'LW_RESOURCE_PROJECT_NOT_FOUND', status: 404 })
      }
      return Promise.resolve({ data: [] })
    })

    const wrapper = mount(ResourceFinanceView)
    await flushPromises()

    expect(wrapper.find('.budget-form').exists()).toBe(false)
    expect(wrapper.find('.budget-card .diagnostic-banner').text()).toContain('RESOURCE_BUDGET_LOAD_FAILED')
  })
})
