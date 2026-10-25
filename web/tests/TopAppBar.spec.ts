import { describe, it, expect, vi, beforeEach } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { getNotificationUnreadCount } from '@/generated/contracts'
import { createPinia } from 'pinia'
import { createMemoryHistory, createRouter } from 'vue-router'
import TopAppBar from '@/components/layout/TopAppBar.vue'
import { refreshUnreadCount } from '@/composables/useNotifications'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, getNotificationUnreadCount: vi.fn() }
})

const authState = vi.hoisted(() => ({
  user: { value: null as { expired: boolean; profile: Record<string, unknown> } | null },
  isLoading: { value: false },
  error: { value: null as Error | null },
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
    authState.error.value = null
    authState.isAuthenticated.value = false
    projectsState.selectedProjectId = null
    vi.mocked(getNotificationUnreadCount).mockReset().mockResolvedValue({ data: { unreadCount: 0 }, error: undefined } as never)
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

  it('shows authentication errors instead of hiding a failed logout', async () => {
    authState.user.value = { expired: false, profile: { roles: ['teacher'] } }
    authState.isAuthenticated.value = true
    authState.error.value = new Error('BFF logout failed with HTTP 503')
    const wrapper = await createWrapper()

    expect(wrapper.get('[role="alert"]').text()).toContain('BFF logout failed with HTTP 503')
  })

  it('emits toggleDrawer when menu button is clicked', async () => {
    const wrapper = await createWrapper()
    await wrapper.find('button[aria-label="打开导航"]').trigger('click')
    expect(wrapper.emitted('toggleDrawer')).toHaveLength(1)
  })

  it('shows the unread count as a badge and in the accessible name', async () => {
    authState.user.value = { expired: false, profile: { roles: ['student'] } }
    authState.isAuthenticated.value = true
    vi.mocked(getNotificationUnreadCount).mockResolvedValue({ data: { unreadCount: 3 }, error: undefined } as never)
    const wrapper = await createWrapper()
    await flushPromises()

    const entry = wrapper.get('[data-testid="notification-entry"]')
    expect(entry.attributes('href')).toBe('/notifications')
    expect(entry.attributes('aria-label')).toBe('通知，3 条未读')
    expect(wrapper.get('[data-testid="notification-unread-badge"]').text()).toBe('3')
    wrapper.unmount()
  })

  it('shows no badge when nothing is unread', async () => {
    authState.user.value = { expired: false, profile: { roles: ['student'] } }
    authState.isAuthenticated.value = true
    const wrapper = await createWrapper()
    await flushPromises()

    expect(wrapper.find('[data-testid="notification-unread-badge"]').exists()).toBe(false)
    expect(wrapper.get('[data-testid="notification-entry"]').attributes('aria-label')).toBe('通知，没有未读')
    wrapper.unmount()
  })

  it('drops a stale count when the unread count cannot be read', async () => {
    authState.user.value = { expired: false, profile: { roles: ['student'] } }
    authState.isAuthenticated.value = true
    vi.mocked(getNotificationUnreadCount).mockResolvedValueOnce({ data: { unreadCount: 5 }, error: undefined } as never)
    const wrapper = await createWrapper()
    await flushPromises()
    expect(wrapper.get('[data-testid="notification-unread-badge"]').text()).toBe('5')

    vi.mocked(getNotificationUnreadCount).mockResolvedValueOnce({
      data: undefined,
      error: { type: 'about:blank', title: 'error', status: 503, detail: 'down', diagnosticCode: 'LW_NOTIFICATION_PERSISTENCE_FAILED', requestId: 'req-1', retryable: true },
    } as never)
    await refreshUnreadCount()
    await flushPromises()

    expect(wrapper.find('[data-testid="notification-unread-badge"]').exists()).toBe(false)
    expect(wrapper.get('[data-testid="notification-entry"]').attributes('aria-label')).toBe('通知，未读数暂时无法读取')
    wrapper.unmount()
  })

  it('offers no notification entry while signed out', async () => {
    const wrapper = await createWrapper()
    await flushPromises()
    expect(wrapper.find('[data-testid="notification-entry"]').exists()).toBe(false)
    expect(getNotificationUnreadCount).not.toHaveBeenCalled()
  })
})
