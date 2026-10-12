import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'
import NavigationDrawer from '@/components/layout/NavigationDrawer.vue'

const authState = vi.hoisted(() => ({
  user: { value: { expired: false, profile: { roles: ['teacher', 'student', 'platform_admin'] } } },
  isAuthenticated: { value: true },
}))

// The compact drawer embeds GcpProjectSelector, which reads the project list state.
const projectsState = vi.hoisted(() => ({
  selectedProjectId: 'project-1',
  selectedProject: null,
  projects: { kind: 'idle' },
  settledProjects: { kind: 'idle' },
  load: () => Promise.resolve(),
  select: () => undefined,
}))

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
    expect(wrapper.findAll('.drawer-item')).toHaveLength(20)
  })

  it('uses project context only for project-scoped destinations', async () => {
    const { wrapper } = await createWrapper()
    const href = (label: string) => wrapper.findAll('a').find((link) => link.text().includes(label))?.attributes('href')

    expect(href('项目与工作空间')).toBe('/researcher/workspaces?projectId=project-1')
    expect(href('资源审批')).toBe('/admin/resource-approval')
    expect(href('GPU 目录')).toBe('/admin/gpu-catalog')
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

  describe('modal (compact viewport)', () => {
    const originalWidth = window.innerWidth
    beforeEach(() => { Object.defineProperty(window, 'innerWidth', { configurable: true, value: 390 }) })
    afterEach(() => { Object.defineProperty(window, 'innerWidth', { configurable: true, value: originalWidth }) })

    async function mountModal(open: boolean) {
      const router = createRouter({ history: createMemoryHistory(), routes: [{ path: '/:pathMatch(.*)*', component: { template: '<div />' } }] })
      await router.push('/')
      await router.isReady()
      const wrapper = mount(NavigationDrawer, { props: { open }, global: { plugins: [router] }, attachTo: document.body })
      await flushPromises()
      return wrapper
    }

    it('renders a modal dialog with focus on the close button and closes on Escape', async () => {
      const wrapper = await mountModal(true)
      const dialog = document.body.querySelector('[role="dialog"]')
      expect(dialog?.classList.contains('navigation-drawer--modal')).toBe(true)
      expect(dialog?.textContent).toContain('应用导航')
      expect(document.activeElement?.getAttribute('aria-label')).toBe('关闭导航')
      expect(wrapper.find('.rail-toggle').exists()).toBe(false)
      document.activeElement?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
      await flushPromises()
      expect(wrapper.emitted('close')).toHaveLength(1)
      wrapper.unmount()
    })

    it('renders nothing while closed', async () => {
      const wrapper = await mountModal(false)
      expect(document.body.querySelector('.navigation-drawer')).toBeNull()
      wrapper.unmount()
    })

    it('offers the project selector inside the compact drawer', async () => {
      const wrapper = await mountModal(true)
      const dialog = document.body.querySelector('[role="dialog"]')
      expect(dialog?.querySelector('.drawer-project .gcp-project-selector')).not.toBeNull()
      wrapper.unmount()
    })
  })

  it('keeps the project selector in the top bar when the drawer is docked', async () => {
    const { wrapper } = await createWrapper()
    expect(wrapper.find('.drawer-project').exists()).toBe(false)
  })
})
