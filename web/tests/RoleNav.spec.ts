import { describe, it, expect } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia } from 'pinia'
import RoleNav from '@/components/navigation/RoleNav.vue'

const createWrapper = () => {
  return mount(RoleNav, {
    global: {
      plugins: [createPinia()],
      stubs: {
        RouterLink: {
          props: ['to'],
          template: '<a :href="to"><slot /></a>',
        },
      },
    },
  })
}

describe('RoleNav', () => {
  it('renders four role navigation links', () => {
    const wrapper = createWrapper()
    const links = wrapper.findAll('.nav-pill')
    expect(links).toHaveLength(4)
  })

  it('displays teacher, student, researcher and admin labels', () => {
    const wrapper = createWrapper()
    const text = wrapper.text()
    expect(text).toContain('教师')
    expect(text).toContain('学生')
    expect(text).toContain('科研')
    expect(text).toContain('管理')
  })
})
