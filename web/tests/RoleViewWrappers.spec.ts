import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import AdminView from '@/views/AdminView.vue'
import ResearcherView from '@/views/ResearcherView.vue'
import StudentView from '@/views/StudentView.vue'

describe('role view wrappers', () => {
  it.each([
    ['student', StudentView],
    ['researcher', ResearcherView],
    ['admin', AdminView],
  ])('keeps the %s route as a plain child outlet', (_role, component) => {
    const wrapper = mount(component, {
      global: {
        stubs: {
          RouterView: { template: '<div data-testid="child-route" />' },
        },
      },
    })

    expect(wrapper.find('[data-testid="child-route"]').exists()).toBe(true)
    expect(wrapper.find('.role-layout').exists()).toBe(false)
  })
})
