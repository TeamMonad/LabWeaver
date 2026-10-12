import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { enableAutoUnmount, flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { defineComponent, h } from 'vue'
import { createMemoryHistory, createRouter, type RouteRecordRaw } from 'vue-router'

const listProjects = vi.hoisted(() => vi.fn())
const getProject = vi.hoisted(() => vi.fn())

vi.mock('@/generated/contracts', () => ({ listProjects, getProject }))

const projectA = { id: 'project-a', name: '项目 A', state: 'active', revision: 1, ownerActorId: 'u', createdAt: '', updatedAt: '' }
const projectB = { id: 'project-b', name: '项目 B', state: 'active', revision: 1, ownerActorId: 'u', createdAt: '', updatedAt: '' }

let mounts = 0
const ScopedPage = defineComponent({
  name: 'ScopedPage',
  setup() {
    mounts += 1
    return () => h('div', { class: 'scoped-page' }, 'project page')
  },
})

const routes: RouteRecordRaw[] = [
  { path: '/', component: ScopedPage, meta: { title: '进行中', projectScoped: true } },
  { path: '/researcher/environments', component: ScopedPage, meta: { title: 'Work 环境', projectScoped: true } },
  { path: '/researcher/workspaces', component: ScopedPage, meta: { title: '项目与工作空间', projectScoped: true } },
  { path: '/admin/gpu-catalog', component: ScopedPage, meta: { title: 'GPU 目录' } },
]

enableAutoUnmount(afterEach)

async function mountShell(path: string) {
  vi.resetModules()
  const { default: AppShell } = await import('@/components/layout/AppShell.vue')
  const router = createRouter({ history: createMemoryHistory(), routes })
  await router.push(path)
  await router.isReady()
  const wrapper = mount(AppShell, {
    global: {
      plugins: [router],
      stubs: { TopAppBar: true, NavigationDrawer: true },
    },
  })
  const { useProjects } = await import('@/composables/useProjects')
  // The real shell loads the catalog through the top bar selector.
  await useProjects().load()
  await flushPromises()
  return { wrapper, router, projects: useProjects() }
}

const problem = (status: number, diagnosticCode: string) => ({
  data: undefined,
  error: { status, diagnosticCode, detail: 'denied', requestId: 'req-ctx', retryable: false, title: '', type: 'about:blank', instance: '/' },
})

describe('AppShell project context', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    localStorage.clear()
    mounts = 0
    listProjects.mockReset()
    getProject.mockReset()
    listProjects.mockResolvedValue({ data: [projectA, projectB], error: undefined })
  })

  it('writes the selected project into the address of a scoped page', async () => {
    localStorage.setItem('labweaver_project_id', 'project-b')
    const { router, wrapper } = await mountShell('/researcher/environments')
    expect(router.currentRoute.value.query.projectId).toBe('project-b')
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
    expect(document.title).toBe('Work 环境 · 项目 B · LabWeaver')
  })

  it('lets a deep link win over the stored selection so title, selector and requests agree', async () => {
    localStorage.setItem('labweaver_project_id', 'project-b')
    const { projects } = await mountShell('/researcher/environments?projectId=project-a')
    expect(projects.selectedProjectId).toBe('project-a')
    expect(document.title).toBe('Work 环境 · 项目 A · LabWeaver')
  })

  it('does not add a project to unscoped admin pages', async () => {
    const { router } = await mountShell('/admin/gpu-catalog')
    expect(router.currentRoute.value.query.projectId).toBeUndefined()
  })

  it('fails closed on an inaccessible linked project and never renders the previous project page', async () => {
    localStorage.setItem('labweaver_project_id', 'project-a')
    getProject.mockResolvedValue(problem(403, 'LW_ACCESS_FORBIDDEN'))
    const { wrapper } = await mountShell('/researcher/environments?projectId=project-x&environmentId=env-old')

    expect(getProject).toHaveBeenCalledWith({ path: { projectId: 'project-x' } })
    expect(wrapper.find('.scoped-page').exists()).toBe(false)
    expect(mounts).toBe(0)
    const recovery = wrapper.get('[data-testid="project-context-recovery"]')
    expect(recovery.text()).toContain('项目不存在或你无权访问。')
    expect(recovery.text()).not.toContain('成员')
    expect(recovery.text()).toContain('req-ctx')
    expect(recovery.text()).toContain('切换到「项目 A」')
    expect(document.title).toContain('项目不可用')
  })

  it('explains a missing project and recovers to an accessible one without carrying object ids', async () => {
    localStorage.setItem('labweaver_project_id', 'project-a')
    getProject.mockResolvedValue(problem(404, 'LW_PROJECT_NOT_FOUND'))
    const { wrapper, router } = await mountShell('/researcher/environments?projectId=project-x&environmentId=env-old')
    // 404 and 403 read the same: the page never asserts whether the project exists.
    expect(wrapper.text()).toContain('项目不存在或你无权访问。')

    await wrapper.get('[data-testid="project-context-recovery"] .filled-button').trigger('click')
    await flushPromises()
    expect(router.currentRoute.value.query.projectId).toBe('project-a')
    expect(router.currentRoute.value.query.environmentId).toBeUndefined()
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
  })

  it('re-creates the page when the project changes so earlier results cannot linger', async () => {
    const { router } = await mountShell('/researcher/environments?projectId=project-a')
    expect(mounts).toBe(1)
    await router.replace({ path: '/researcher/environments', query: { projectId: 'project-b' } })
    await flushPromises()
    expect(mounts).toBe(2)
  })
})
