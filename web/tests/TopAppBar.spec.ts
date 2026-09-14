import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia } from 'pinia'
import { createMemoryHistory, createRouter } from 'vue-router'
import TopAppBar from '@/components/layout/TopAppBar.vue'

const authState = vi.hoisted(() => ({
  user: { value: null as { expired: boolean; profile: Record<string, unknown> } | null },
  isLoading: { value: false },
  isAuthenticated: { value: false },
  login: vi.fn(),
  logout: vi.fn(),
}))

const projectsState = vi.hoisted(() => ({
  projects: { kind: 'empty' as const },
  selectedProjectId: null as string | null,
  selectedProject: null,
}))

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
  return mount(TopAppBar, {
    props: { drawerOpen: false },
    global: {
      plugins: [createPinia(), router],
      stubs: {
        RouterLink: {
          props: ['to'],
          template: '<a :href="typeof to === \'string\' ? to : to.path"><slot /></a>',
        },
      },
    },
  })
}

describe('TopAppBar', () => {
  beforeEach(() => {
    authState.user.value = null
    authState.isLoading.value = false
    authState.isAuthenticated.value = false
    projectsState.selectedProjectId = null
  })

  it('shows unauthenticated state and login button', async () => {
    const wrapper = await createWrapper()
    expect(wrapper.text()).toContain('未认证')
    expect(wrapper.text()).toContain('登录')
    expect(wrapper.find('.shell-button').exists()).toBe(false)
  })

  it('opens the shared Work console for a teacher', async () => {
    authState.user.value = { expired: false, profile: { roles: ['teacher'], name: '张老师' } }
    authState.isAuthenticated.value = true
    projectsState.selectedProjectId = 'project-1'
    const wrapper = await createWrapper()

    expect(wrapper.find('.shell-button').attributes('href')).toBe('/researcher/environments?projectId=project-1')
    expect(wrapper.text()).toContain('张老师')
  })

  it('does not show an internal actor id as the display name', async () => {
    authState.user.value = { expired: false, profile: { roles: ['student'], actor_id: 'actor-1234567890' } }
    authState.isAuthenticated.value = true
    const wrapper = await createWrapper()

    expect(wrapper.text()).toContain('已登录用户')
    expect(wrapper.text()).not.toContain('actor-1234567890')
  })

  it('emits toggleDrawer when menu button is clicked', async () => {
    const wrapper = await createWrapper()
    await wrapper.find('button[aria-label="打开导航"]').trigger('click')
    expect(wrapper.emitted('toggleDrawer')).toHaveLength(1)
  })
})
