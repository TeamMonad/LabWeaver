import { describe, it, expect } from 'vitest'
import { mount } from '@vue/test-utils'
import { createRouter, createWebHistory } from 'vue-router'
import NavigationDrawer from '@/components/layout/NavigationDrawer.vue'

const createWrapper = () => {
  const router = createRouter({
    history: createWebHistory(),
    routes: [
      { path: '/', component: { template: '<div>home</div>' } },
      { path: '/teacher', component: { template: '<div>teacher</div>' } },
      { path: '/student', component: { template: '<div>student</div>' } },
      { path: '/researcher', component: { template: '<div>researcher</div>' } },
      { path: '/admin', component: { template: '<div>admin</div>' } },
    ],
  })

  return mount(NavigationDrawer, {
    props: { open: true },
    global: {
      plugins: [router],
    },
  })
}

describe('NavigationDrawer', () => {
  it('renders four role navigation links', () => {
    const wrapper = createWrapper()
    const links = wrapper.findAll('.drawer-item')
    expect(links).toHaveLength(4)
  })

  it('displays teacher, student, researcher and admin labels', () => {
    const wrapper = createWrapper()
    const text = wrapper.text()
    expect(text).toContain('教师工作台')
    expect(text).toContain('学生工作台')
    expect(text).toContain('科研工作台')
    expect(text).toContain('管理工作台')
  })
})
