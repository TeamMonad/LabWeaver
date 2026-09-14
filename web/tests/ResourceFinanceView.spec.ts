import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import ResourceFinanceView from '@/views/admin/ResourceFinanceView.vue'

const api = vi.hoisted(() => ({
  get: vi.fn(),
  put: vi.fn(),
  post: vi.fn(),
}))

interface TestProject {
  id: string
  name: string
  courseId: string | null
}

type ProjectListState =
  | { kind: 'loading'; message: string }
  | { kind: 'success'; data: TestProject[] }

interface TestProjectsState {
  projects: ProjectListState
  selectedProjectId: string | null
  selectedProject: TestProject | null
  select: (id: string) => void
  load: () => void
}

const projectMocks = vi.hoisted(() => ({
  state: null as TestProjectsState | null,
  catalog: [] as TestProject[],
}))

const routerMocks = vi.hoisted(() => ({
  route: { query: {} as Record<string, string | undefined> },
  replace: vi.fn(),
}))

vi.mock('@/api/client', () => ({ apiClient: api }))

vi.mock('@/composables/useProjects', async () => {
  const { reactive } = await import('vue')
  const projectNew = { id: 'project-new', name: '新项目', courseId: null }
  const projectOther = { id: 'project-other', name: '另一个项目', courseId: 'course-2' }
  projectMocks.catalog = [projectNew, projectOther]
  const state = reactive({
    projects: {
      kind: 'success' as const,
      data: projectMocks.catalog,
    } as ProjectListState,
    selectedProjectId: projectNew.id as string | null,
    selectedProject: projectNew as TestProject | null,
    select(id: string) {
      state.selectedProjectId = id
      state.selectedProject = projectMocks.catalog.find((project) => project.id === id) ?? null
    },
    load: vi.fn(),
  })
  projectMocks.state = state
  return { useProjects: () => state }
})

vi.mock('vue-router', async (importOriginal) => {
  const actual = await importOriginal<typeof import('vue-router')>()
  const { reactive } = await import('vue')
  routerMocks.route = reactive(routerMocks.route)
  return {
    ...actual,
    useRoute: () => routerMocks.route,
    useRouter: () => ({ replace: routerMocks.replace }),
  }
})

const mountedViews: Array<{ unmount: () => void }> = []

function mountView() {
  const wrapper = mount(ResourceFinanceView, {
    global: { stubs: { RouterLink: { props: ['to'], template: '<a :data-path="typeof to === \'string\' ? to : to.path"><slot /></a>' } } },
  })
  mountedViews.push(wrapper)
  return wrapper
}

describe('ResourceFinanceView', () => {
  afterEach(() => {
    for (const wrapper of mountedViews.splice(0)) wrapper.unmount()
  })

  beforeEach(() => {
    api.get.mockReset()
    routerMocks.replace.mockReset()
    routerMocks.replace.mockImplementation(({ query }: { query: Record<string, string | undefined> }) => {
      routerMocks.route.query = query
    })
    routerMocks.route.query = {}
    projectMocks.state!.projects = { kind: 'success', data: projectMocks.catalog }
    projectMocks.state!.selectedProjectId = 'project-new'
    projectMocks.state!.selectedProject = projectMocks.catalog[0]
  })

  it('shows the editable create form when Resource reports a missing project budget', async () => {
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) {
        // Resource uses this stable plain-text 404 to indicate an unconfigured budget.
        return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      }
      return Promise.resolve({ data: [] })
    })

    const wrapper = mountView()
    await flushPromises()

    expect(wrapper.find('.budget-form').exists()).toBe(true)
    expect(wrapper.find('.budget-form button[type="submit"]').text()).toBe('创建预算')
    expect(wrapper.find('.budget-card .diagnostic-banner').exists()).toBe(false)
  })

  it('keeps an unrelated 404 as a budget load error', async () => {
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) {
        return Promise.resolve({ error: 'LW_RESOURCE_PROJECT_NOT_FOUND', status: 404 })
      }
      return Promise.resolve({ data: [] })
    })

    const wrapper = mountView()
    await flushPromises()

    expect(wrapper.find('.budget-form').exists()).toBe(false)
    expect(wrapper.find('.budget-card .diagnostic-banner').text()).toContain('RESOURCE_BUDGET_LOAD_FAILED')
  })

  it('uses the URL project and resets budget and adjustment drafts when the shared project changes', async () => {
    routerMocks.route.query = { projectId: 'project-other' }
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      return Promise.resolve({ data: [] })
    })

    const wrapper = mountView()
    await flushPromises()

    const projectSelect = wrapper.get('select')
    expect((projectSelect.element as HTMLSelectElement).value).toBe('project-other')
    expect(api.get).toHaveBeenCalledWith(expect.objectContaining({ url: '/api/v1/projects/project-other/resource-budget' }))

    const limitInput = wrapper.get('input[inputmode="decimal"]')
    await limitInput.setValue('12.000000')
    await projectSelect.setValue('project-new')
    await flushPromises()

    expect(projectMocks.state!.selectedProjectId).toBe('project-new')
    expect((wrapper.get('input[inputmode="decimal"]').element as HTMLInputElement).value).toBe('0.000000')
    expect(routerMocks.replace).toHaveBeenCalledWith({ query: { projectId: 'project-new' } })
  })

  it('blocks an unavailable URL project instead of silently switching to another project', async () => {
    routerMocks.route.query = { projectId: 'project-missing' }
    api.get.mockResolvedValue({ data: [] })

    const wrapper = mountView()
    await flushPromises()

    expect(wrapper.text()).toContain('PROJECT_CONTEXT_UNAVAILABLE')
    expect(wrapper.text()).toContain('不存在或你无权访问')
    expect(api.get).not.toHaveBeenCalled()
    expect(wrapper.get('a[data-path="/researcher/workspaces"]').exists()).toBe(true)

    await wrapper.get('select').setValue('project-new')
    await flushPromises()

    expect(wrapper.text()).not.toContain('PROJECT_CONTEXT_UNAVAILABLE')
    expect(routerMocks.route.query).toMatchObject({ projectId: 'project-new' })
  })

  it('keeps an unavailable URL blocked when the project list resolves and preserves its first selection', async () => {
    routerMocks.route.query = { projectId: 'project-missing' }
    const state = projectMocks.state!
    state.projects = { kind: 'loading', message: '加载项目…' }
    state.selectedProjectId = null
    state.selectedProject = null
    api.get.mockResolvedValue({ data: [] })

    const wrapper = mountView()
    expect(api.get).not.toHaveBeenCalled()

    state.projects = { kind: 'success', data: projectMocks.catalog }
    state.selectedProjectId = 'project-new'
    state.selectedProject = projectMocks.catalog[0]
    await flushPromises()

    expect(state.selectedProjectId).toBe('project-new')
    expect(routerMocks.replace).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('PROJECT_CONTEXT_UNAVAILABLE')
    expect(api.get).not.toHaveBeenCalled()
  })

  it('selects the first project and loads it after the project list finishes loading', async () => {
    const state = projectMocks.state!
    state.projects = { kind: 'loading', message: '加载项目…' }
    state.selectedProjectId = null
    state.selectedProject = null
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      return Promise.resolve({ data: [] })
    })

    const wrapper = mountView()
    expect(api.get).not.toHaveBeenCalled()

    state.projects = { kind: 'success', data: projectMocks.catalog }
    await flushPromises()

    expect(state.selectedProjectId).toBe('project-new')
    expect(api.get).toHaveBeenCalledWith(expect.objectContaining({ url: '/api/v1/projects/project-new/resource-budget' }))
    expect(routerMocks.route.query).toMatchObject({ projectId: 'project-new' })
    expect(wrapper.get('select').element).toHaveProperty('value', 'project-new')
  })
})
