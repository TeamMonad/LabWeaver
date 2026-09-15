import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'
import HomeView from '@/views/HomeView.vue'

const authState = vi.hoisted(() => ({
  user: { value: null as { expired: boolean; profile: Record<string, unknown> } | null },
  isLoading: { value: false },
  isAuthenticated: { value: false },
  login: vi.fn(),
}))

const projectsState = vi.hoisted(() => ({ selectedProjectId: 'project-1' as string | null }))

vi.mock('@/composables/useAuth', () => ({
  useAuth: () => authState,
}))

vi.mock('@/composables/useProjects', () => ({
  useProjects: () => projectsState,
}))

vi.mock('@/config', () => ({
  OIDC_ENABLED: true,
  API_BASE_URL: '/api/v1',
  APP_TITLE: 'LabWeaver',
}))

async function createWrapper() {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/:pathMatch(.*)*', component: { template: '<div />' } }],
  })
  await router.push('/')
  await router.isReady()
  const wrapper = mount(HomeView, { global: { plugins: [router] } })
  return { wrapper, router }
}

describe('HomeView', () => {
  beforeEach(() => {
    authState.user.value = { expired: false, profile: { roles: ['teacher', 'student', 'platform_admin'] } }
    authState.isLoading.value = false
    authState.isAuthenticated.value = true
    projectsState.selectedProjectId = 'project-1'
  })

  it('presents concurrent task groups instead of a role chooser', async () => {
    const { wrapper } = await createWrapper()

    expect(wrapper.findAll('[data-task-group]')).toHaveLength(4)
    expect(wrapper.text()).toContain('教学管理')
    expect(wrapper.text()).toContain('我的实验')
    expect(wrapper.text()).toContain('项目与工作')
    expect(wrapper.text()).toContain('平台管理')
    expect(wrapper.text()).not.toContain('选择角色入口')
    expect(wrapper.find('.status-bar').exists()).toBe(false)
    expect(wrapper.findAll('a').some((link) => link.attributes('href') === '/admin/resource-approval')).toBe(true)
  })

  it('shows only the task groups authorized for the current platform roles', async () => {
    authState.user.value = { expired: false, profile: { roles: ['teacher'] } }
    const { wrapper } = await createWrapper()

    expect(wrapper.text()).toContain('教学管理')
    expect(wrapper.text()).toContain('项目与工作')
    expect(wrapper.text()).not.toContain('我的实验')
    expect(wrapper.text()).not.toContain('平台管理')
  })

  it('does not render authorized tasks for an expired session', async () => {
    authState.user.value = { expired: true, profile: { roles: ['teacher', 'student', 'platform_admin'] } }
    authState.isAuthenticated.value = false
    const { wrapper } = await createWrapper()

    expect(wrapper.findAll('[data-task-group]')).toHaveLength(0)
    expect(wrapper.text()).toContain('登录 LabWeaver')
    expect(wrapper.text()).toContain('请使用组织账号登录')
  })
})
