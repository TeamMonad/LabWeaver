import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { enableAutoUnmount, flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { defineComponent, h } from 'vue'
import { createMemoryHistory, createRouter, type RouteRecordRaw } from 'vue-router'
// Each test re-imports the shell after vi.resetModules() so module-level
// project state starts fresh. Loading the graph once here moves the cold
// transform of the shell and its layout components into file collection,
// which has no per-test timeout; the per-test re-import only re-evaluates.
import '@/components/layout/AppShell.vue'

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
    listProjects.mockResolvedValue({ data: { items: [projectA, projectB], nextCursor: null }, error: undefined })
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

  it('fails closed when the project list cannot be loaded and resolves the link only after a successful retry', async () => {
    localStorage.setItem('labweaver_project_id', 'project-b')
    listProjects.mockResolvedValue(problem(503, 'LW_HTTP_SERVICE_UNAVAILABLE'))
    getProject.mockResolvedValue(problem(404, 'LW_PROJECT_NOT_FOUND'))
    const { wrapper } = await mountShell('/researcher/environments?projectId=project-x')

    expect(wrapper.find('[data-testid="project-context-list-error"]').exists()).toBe(true)
    expect(wrapper.find('.scoped-page').exists()).toBe(false)
    expect(mounts).toBe(0)
    expect(getProject).not.toHaveBeenCalled()
    expect(document.title).toContain('项目不可用')

    listProjects.mockResolvedValue({ data: { items: [projectA, projectB], nextCursor: null }, error: undefined })
    await wrapper.get('[data-testid="project-context-list-error"] .diagnostic-banner > button.text-button').trigger('click')
    await flushPromises()
    await flushPromises()
    expect(wrapper.find('[data-testid="project-context-list-error"]').exists()).toBe(false)
    // project-x is still not listed, so the shell shows recovery, never project B's page.
    expect(wrapper.get('[data-testid="project-context-recovery"]').text()).toContain('项目不存在或你无权访问。')
    expect(mounts).toBe(0)
  })

  it('treats a readable project missing from a reloaded list as unavailable instead of pairing it with the old selection', async () => {
    localStorage.setItem('labweaver_project_id', 'project-b')
    getProject.mockResolvedValue({ data: { ...projectA, id: 'project-x', name: '项目 X' }, error: undefined })
    const { wrapper, projects } = await mountShell('/?projectId=project-x')
    await flushPromises()

    // The list is reloaded before asking the server why the project is missing, and the reloads settle.
    const listCalls = listProjects.mock.calls.length
    const probeCalls = getProject.mock.calls.length
    expect(probeCalls).toBeGreaterThanOrEqual(1)
    await flushPromises()
    expect(listProjects.mock.calls.length).toBe(listCalls)
    expect(getProject.mock.calls.length).toBe(probeCalls)
    expect(wrapper.find('.scoped-page').exists()).toBe(false)
    expect(mounts).toBe(0)
    expect(wrapper.get('[data-testid="project-context-recovery"]').text()).toContain('这个项目不在你的项目列表中')
    expect(projects.selectedProjectId).toBe('project-b')
    expect(document.title).toContain('项目不可用')
  })

  it('asks again after the list changes instead of reusing an earlier denial', async () => {
    getProject.mockResolvedValue(problem(403, 'LW_ACCESS_FORBIDDEN'))
    const { wrapper, projects } = await mountShell('/researcher/environments?projectId=project-x')
    expect(wrapper.find('[data-testid="project-context-recovery"]').exists()).toBe(true)

    const projectX = { ...projectA, id: 'project-x', name: '项目 X' }
    listProjects.mockResolvedValue({ data: { items: [projectA, projectB, projectX], nextCursor: null }, error: undefined })
    await projects.load()
    await flushPromises()
    expect(wrapper.find('[data-testid="project-context-recovery"]').exists()).toBe(false)
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
    expect(projects.selectedProjectId).toBe('project-x')
  })

  it('follows the cursor to a linked project beyond the first page instead of reporting it unavailable', async () => {
    const projectX = { ...projectA, id: 'project-x', name: '项目 X' }
    listProjects.mockImplementation(async (options: { query?: { cursor?: string } }) => (
      options.query?.cursor === 'page-2'
        ? { data: { items: [projectX], nextCursor: null }, error: undefined }
        : { data: { items: [projectA, projectB], nextCursor: 'page-2' }, error: undefined }
    ))
    const { wrapper, projects } = await mountShell('/researcher/environments?projectId=project-x')
    await flushPromises()

    expect(listProjects).toHaveBeenCalledWith({ query: { limit: 50, cursor: 'page-2' } })
    expect(getProject).not.toHaveBeenCalled()
    expect(wrapper.find('[data-testid="project-context-recovery"]').exists()).toBe(false)
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
    expect(projects.selectedProjectId).toBe('project-x')
    expect(document.title).toBe('Work 环境 · 项目 X · LabWeaver')
  })

  it('keeps the loaded pages when locating a linked project fails midway and continues locating on retry', async () => {
    const projectC = { ...projectA, id: 'project-c', name: '项目 C' }
    const projectX = { ...projectA, id: 'project-x', name: '项目 X' }
    let pageThreeFails = true
    listProjects.mockImplementation(async (options: { query?: { cursor?: string } }) => {
      if (options.query?.cursor === 'page-2') return { data: { items: [projectC], nextCursor: 'page-3' }, error: undefined }
      if (options.query?.cursor === 'page-3') {
        return pageThreeFails
          ? { ...problem(503, 'CONTROL_UNAVAILABLE'), error: { ...problem(503, 'CONTROL_UNAVAILABLE').error, retryable: true } }
          : { data: { items: [projectX], nextCursor: null }, error: undefined }
      }
      return { data: { items: [projectA, projectB], nextCursor: 'page-2' }, error: undefined }
    })
    const { wrapper, router, projects } = await mountShell('/researcher/environments?projectId=project-x')
    // Imported after mountShell's vi.resetModules() so it shares this test's fresh project state.
    const { default: GcpProjectSelector } = await import('@/components/layout/GcpProjectSelector.vue')
    const selector = mount(GcpProjectSelector, { global: { plugins: [router] } })
    await flushPromises()

    // The failed third page is a retryable list error; pages one and two stay loaded.
    expect(listProjects).toHaveBeenLastCalledWith({ query: { limit: 50, cursor: 'page-3' } })
    expect(getProject).not.toHaveBeenCalled()
    expect(projects.projects.kind === 'success' ? projects.projects.data.map((project) => project.id) : []).toEqual(['project-a', 'project-b', 'project-c'])
    expect(wrapper.find('[data-testid="project-context-list-error"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="project-context-recovery"]').exists()).toBe(false)
    expect(wrapper.find('.scoped-page').exists()).toBe(false)
    expect(selector.find('.trigger-primary').text()).not.toBe('项目不可用')
    expect(router.currentRoute.value.query.projectId).toBe('project-x')

    pageThreeFails = false
    const callsBeforeRetry = listProjects.mock.calls.length
    await wrapper.get('[data-testid="project-context-list-error"] .diagnostic-retry').trigger('click')
    await flushPromises()

    // The retry continues from the failed page instead of restarting at page one.
    expect(listProjects.mock.calls.slice(callsBeforeRetry)).toEqual([[{ query: { limit: 50, cursor: 'page-3' } }]])
    expect(projects.projects.kind === 'success' ? projects.projects.data.map((project) => project.id) : []).toEqual(['project-a', 'project-b', 'project-c', 'project-x'])
    expect(projects.hasMore).toBe(false)
    expect(getProject).not.toHaveBeenCalled()
    expect(router.currentRoute.value.query.projectId).toBe('project-x')
    expect(projects.selectedProjectId).toBe('project-x')
    expect(selector.find('.trigger-primary').text()).toBe('项目 X')
    expect(wrapper.find('[data-testid="project-context-list-error"]').exists()).toBe(false)
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
    expect(document.title).toBe('Work 环境 · 项目 X · LabWeaver')
    selector.unmount()
  })
})
