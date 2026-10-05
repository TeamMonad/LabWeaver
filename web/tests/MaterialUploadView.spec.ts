import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { createMemoryHistory, createRouter } from 'vue-router'
import MaterialUploadView from '@/views/teacher/MaterialUploadView.vue'
import {
  getActiveProjectLlmPolicy,
  getProjectAgentRun,
  getProjectProblemPackage,
  listProjectAgentRuns,
  listProjects,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getActiveProjectLlmPolicy: vi.fn(),
    getProjectAgentRun: vi.fn(),
    getProjectProblemPackage: vi.fn(),
    listProjectAgentRuns: vi.fn(),
    listProjects: vi.fn(),
  }
})

const mockProject = {
  id: 'project-1',
  name: 'Demo project',
  description: 'Project used by the upload view test',
  ownerActorId: 'teacher-1',
  courseId: 'course-1',
  revision: 1,
  state: 'active',
  createdAt: '2026-07-11T00:00:00.000Z',
  updatedAt: '2026-07-11T00:00:00.000Z',
}

const mockPolicy = {
  id: 'policy-1',
  projectId: 'project-1',
  courseId: 'course-1',
  revision: 3,
  activatedAt: '2026-07-11T00:00:00.000Z',
  binding: {
    runtimeBinding: 'demo-binding',
    model: 'claude-3-5-sonnet',
    claudeCodeVersion: '0.1.0',
    workerImageSha256: 'a'.repeat(64),
    runtimeConfigSha256: 'b'.repeat(64),
    maxInFlightPerWorker: 4,
  },
  deniedDataClasses: ['secret', 'token', 'private_key'],
  budget: {
    maxInputTokens: 100000,
    maxOutputTokens: 20000,
    maxRequests: 50,
    maxCostMicrousd: 1000000,
    timeoutMilliseconds: 120000,
    maxTransientRetries: 3,
    maxSchemaRepairs: 2,
  },
  studentContentMode: 'manifest_allowlist_only',
}

const mockPackage = {
  id: 'package-1',
  projectId: 'project-1',
  courseId: 'course-1',
  revision: 2,
  files: [],
  retention: {},
  completedAt: '2026-07-11T00:00:00.000Z',
}

const experimentRun = {
  id: 'experiment-run-1',
  projectId: 'project-1',
  courseId: 'course-1',
  actorId: 'teacher-1',
  packageId: 'package-1',
  policyId: 'policy-1',
  policyRevision: 3,
  revision: 4,
  purpose: { kind: 'authoring', environmentClass: 'experiment' },
  state: 'failed',
  tracks: [],
  plan: null,
  createdAt: '2026-07-11T00:00:00.000Z',
  updatedAt: '2026-07-11T00:01:00.000Z',
}

const experimentHistoryItem = {
  id: 'experiment-run-1',
  projectId: 'project-1',
  purpose: { kind: 'authoring', environmentClass: 'experiment' },
  state: 'failed',
  createdAt: '2026-07-11T00:00:00.000Z',
  updatedAt: '2026-07-11T00:01:00.000Z',
}

describe('MaterialUploadView', () => {
  async function mountView() {
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [{ path: '/teacher/materials', component: MaterialUploadView }],
    })
    await router.push('/teacher/materials')
    await router.isReady()

    const wrapper = mount(MaterialUploadView, {
      global: { plugins: [router] },
    })
    return { router, wrapper }
  }

  beforeEach(() => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
    vi.mocked(listProjectAgentRuns).mockResolvedValue({
      data: { items: [], page: 1, pageSize: 100, hasMore: false } as never,
      error: undefined as never,
    })
  })

  it('shows a project-context diagnostic when no accessible project is returned', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [], error: undefined as never })

    const { wrapper } = await mountView()
    await vi.waitFor(() => expect(wrapper.text()).toContain('PROJECT_CONTEXT_MISSING'))
    expect(wrapper.text()).toContain('请先在顶部项目选择器中选择一个项目。')
    expect(getActiveProjectLlmPolicy).not.toHaveBeenCalled()
  })

  it('loads the active project policy for the selected project', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject] as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({ data: mockPolicy as never, error: undefined as never })

    const { wrapper } = await mountView()
    await vi.waitFor(() => expect(wrapper.text()).toContain('claude-3-5-sonnet'))
    expect(wrapper.text()).toContain('secret')
    expect(wrapper.text()).toContain('rev-3 / policy-1')
    expect(getActiveProjectLlmPolicy).toHaveBeenCalledWith({ path: { projectId: 'project-1' } })
  })

  it('surfaces project policy errors as a diagnostic', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject] as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({
      data: undefined as never,
      error: {
        response: {
          data: {
            diagnosticCode: 'LW_ACCESS_DENIED',
            detail: '无策略读取权限',
            retryable: false,
          },
        },
      } as never,
    })

    const { wrapper } = await mountView()
    await vi.waitFor(() => expect(wrapper.text()).toContain('无策略读取权限'))
    expect(wrapper.text()).toContain('LW_ACCESS_DENIED')
  })

  it('links to project AI settings when the active policy is missing', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject] as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({
      data: undefined as never,
      error: {
        response: {
          status: 404,
          data: { diagnosticCode: 'LW_POLICY_NOT_FOUND', detail: 'no active policy' },
        },
      } as never,
    })

    const { wrapper } = await mountView()
    await vi.waitFor(() => expect(wrapper.find('[data-testid="material-policy-missing"]').exists()).toBe(true))
    expect(wrapper.find('[data-testid="material-policy-missing"] a').attributes('href')).toBe('/researcher/ai-policy?projectId=project-1')
  })

  it('opens an experiment task from history and restores its package', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject] as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({ data: mockPolicy as never, error: undefined as never })
    vi.mocked(listProjectAgentRuns).mockResolvedValue({
      data: { items: [experimentHistoryItem], page: 1, pageSize: 100, hasMore: false } as never,
      error: undefined as never,
    })
    vi.mocked(getProjectAgentRun).mockResolvedValue({ data: experimentRun as never, error: undefined as never })
    vi.mocked(getProjectProblemPackage).mockResolvedValue({ data: mockPackage as never, error: undefined as never })

    const router = createRouter({
      history: createMemoryHistory(),
      routes: [{ path: '/teacher/materials', component: MaterialUploadView }],
    })
    await router.push('/teacher/materials?projectId=project-1')
    await router.isReady()
    const wrapper = mount(MaterialUploadView, { global: { plugins: [router] } })

    await vi.waitFor(() => expect(wrapper.find('[data-testid="project-agent-run-history"] button.outlined-button').exists()).toBe(true))
    await wrapper.get('[data-testid="project-agent-run-history"] button.outlined-button').trigger('click')

    await vi.waitFor(() => expect(getProjectAgentRun).toHaveBeenCalledWith({ path: { projectId: 'project-1', runId: 'experiment-run-1' } }))
    await vi.waitFor(() => expect(getProjectProblemPackage).toHaveBeenCalledWith({ path: { projectId: 'project-1', packageId: 'package-1' } }))
    expect(router.currentRoute.value.query).toMatchObject({ projectId: 'project-1', runId: 'experiment-run-1', packageId: 'package-1' })
    expect(wrapper.text()).toContain('生成失败')
    wrapper.unmount()
  })

  it('ignores a late experiment history response after the project changes', async () => {
    const project2 = { ...mockProject, id: 'project-2', name: 'Second project' }
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject, project2] as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({ data: mockPolicy as never, error: undefined as never })
    let resolveFirst!: (value: unknown) => void
    vi.mocked(listProjectAgentRuns).mockImplementation((options) => {
      const projectId = (options as { path: { projectId: string } }).path.projectId
      if (projectId === 'project-1') return new Promise((resolve) => { resolveFirst = resolve }) as never
      return Promise.resolve({ data: { items: [], page: 1, pageSize: 100, hasMore: false }, error: undefined }) as never
    })

    const router = createRouter({
      history: createMemoryHistory(),
      routes: [{ path: '/teacher/materials', component: MaterialUploadView }],
    })
    await router.push('/teacher/materials?projectId=project-1')
    await router.isReady()
    const wrapper = mount(MaterialUploadView, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(listProjectAgentRuns).toHaveBeenCalledWith(expect.objectContaining({ path: { projectId: 'project-1' } })))

    await router.replace({ query: { projectId: 'project-2' } })
    await vi.waitFor(() => expect(listProjectAgentRuns).toHaveBeenCalledWith(expect.objectContaining({ path: { projectId: 'project-2' } })))
    resolveFirst({ data: { items: [experimentHistoryItem], page: 1, pageSize: 100, hasMore: false }, error: undefined })
    await Promise.resolve()

    expect(wrapper.find('[data-testid="project-agent-run-history"] .run-history-item').exists()).toBe(false)
    wrapper.unmount()
  })

  it('rejects a history item whose loaded run has another purpose', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject] as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({ data: mockPolicy as never, error: undefined as never })
    vi.mocked(listProjectAgentRuns).mockResolvedValue({
      data: { items: [experimentHistoryItem], page: 1, pageSize: 100, hasMore: false } as never,
      error: undefined as never,
    })
    vi.mocked(getProjectAgentRun).mockResolvedValue({
      data: { ...experimentRun, purpose: { kind: 'work_configuration', environmentId: 'environment-1', environmentRevision: 1, runtimeKind: 'container', actorId: 'teacher-1' } } as never,
      error: undefined as never,
    })

    const router = createRouter({
      history: createMemoryHistory(),
      routes: [{ path: '/teacher/materials', component: MaterialUploadView }],
    })
    await router.push('/teacher/materials?projectId=project-1')
    await router.isReady()
    const wrapper = mount(MaterialUploadView, { global: { plugins: [router] } })

    await vi.waitFor(() => expect(wrapper.find('[data-testid="project-agent-run-history"] button.outlined-button').exists()).toBe(true))
    await wrapper.get('[data-testid="project-agent-run-history"] button.outlined-button').trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('该运行记录不是实验包生成任务'))
    expect(getProjectProblemPackage).not.toHaveBeenCalled()
    wrapper.unmount()
  })
})
