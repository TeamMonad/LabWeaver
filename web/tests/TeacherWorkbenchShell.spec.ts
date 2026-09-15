import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import TeacherWorkbenchShell from '@/components/teacher/TeacherWorkbenchShell.vue'

describe('TeacherWorkbenchShell', () => {
  it('leaves navigation and page framing to the unified app shell', () => {
    const wrapper = mount(TeacherWorkbenchShell, {
      global: {
        stubs: {
          RouterView: { template: '<div data-testid="teacher-route-view" />' },
        },
      },
    })

    expect(wrapper.find('[data-testid="teacher-route-view"]').exists()).toBe(true)
    expect(wrapper.find('.module-nav').exists()).toBe(false)
    expect(wrapper.find('.resource-tree').exists()).toBe(false)
    expect(wrapper.find('.workspace-header').exists()).toBe(false)
    expect(wrapper.text()).not.toContain('尚未绑定')
  })
})
