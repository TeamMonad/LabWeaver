import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'
import NavigationDrawer from '@/components/layout/NavigationDrawer.vue'

const authState = vi.hoisted(() => ({
  user: { value: { expired: false, profile: { roles: ['teacher', 'student', 'platform_admin'] } } },
  isAuthenticated: { value: true },
}))

const projectsState = vi.hoisted(() => ({ selectedProjectId: 'project-1' }))

vi.mock('@/composables/useAuth', () => ({
  useAuth: () => authState,
}))

vi.mock('@/composables/useProjects', () => ({
  useProjects: () => projectsState,
}))

async function createWrapper() {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/:pathMatch(.*)*', component: { template: '<div />' } }],
  })
  await router.push('/')
  await router.isReady()
  const wrapper = mount(NavigationDrawer, {
    props: { open: true },
    global: { plugins: [router] },
  })
  return { wrapper, router }
}

describe('NavigationDrawer', () => {
  beforeEach(() => {
    authState.user.value = { expired: false, profile: { roles: ['teacher', 'student', 'platform_admin'] } }
    authState.isAuthenticated.value = true
    projectsState.selectedProjectId = 'project-1'
  })

  it('shows every authorized task group at the same time', async () => {
    const { wrapper } = await createWrapper()

    expect(wrapper.findAll('[data-nav-group]')).toHaveLength(4)
    expect(wrapper.text()).toContain('教学管理')
    expect(wrapper.text()).toContain('我的实验')
    expect(wrapper.text()).toContain('项目与工作')
    expect(wrapper.text()).toContain('平台管理')
    expect(wrapper.text()).not.toContain('工作台角色')
    expect(wrapper.findAll('.drawer-item')).toHaveLength(18)
  })

  it('uses project context only for project-scoped destinations', async () => {
    const { wrapper } = await createWrapper()
    const href = (label: string) => wrapper.findAll('a').find((link) => link.text().includes(label))?.attributes('href')

    expect(href('项目与工作空间')).toBe('/researcher/workspaces?projectId=project-1')
    expect(href('资源审批')).toBe('/admin/resource-approval')
  })

  it('does not expose another role’s task group', async () => {
    authState.user.value = { expired: false, profile: { roles: ['teacher'] } }
    const { wrapper } = await createWrapper()

    expect(wrapper.text()).toContain('教学管理')
    expect(wrapper.text()).toContain('项目与工作')
    expect(wrapper.text()).not.toContain('我的实验')
    expect(wrapper.text()).not.toContain('平台管理')
  })

  it('does not retain task links from an expired session', async () => {
    authState.user.value = { expired: true, profile: { roles: ['teacher', 'student', 'platform_admin'] } }
    authState.isAuthenticated.value = false
    const { wrapper } = await createWrapper()

    expect(wrapper.findAll('[data-nav-group]')).toHaveLength(0)
    expect(wrapper.findAll('.drawer-item')).toHaveLength(0)
    expect(wrapper.text()).toContain('登录后显示可用任务')
  })
})
