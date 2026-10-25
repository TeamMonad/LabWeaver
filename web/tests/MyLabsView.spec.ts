import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createMemoryHistory, createRouter } from 'vue-router'
import { reactive } from 'vue'
import MyLabsView from '@/views/student/MyLabsView.vue'
import { useAuth } from '@/composables/useAuth'
import {
  createEnvironment,
  listEnvironmentTemplateReleases,
  listEnvironments,
  listProjectResourceLeases,
  listProjectResourceRequests,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    listEnvironmentTemplateReleases: vi.fn(),
    listEnvironments: vi.fn(),
    listProjectResourceRequests: vi.fn(),
    listProjectResourceLeases: vi.fn(),
    createEnvironment: vi.fn(),
    startEnvironment: vi.fn(),
    stopEnvironment: vi.fn(),
    restartEnvironment: vi.fn(),
    deleteEnvironment: vi.fn(),
  }
})

const projectOne = {
  id: 'project-1',
  name: '课程项目',
  description: null,
  ownerActorId: 'teacher-1',
  courseId: 'course-1',
  state: 'active',
  revision: 1,
  createdAt: '2026-09-01T00:00:00.000Z',
  updatedAt: '2026-09-01T00:00:00.000Z',
}

const projectTwo = { ...projectOne, id: 'project-2', name: 'Work 项目', courseId: null }

const experimentRelease = {
  id: 'release-exp',
  projectId: 'project-1',
  courseId: 'course-1',
  version: 1,
  purpose: 'experiment',
  name: '缓冲区实验',
  runtimeKind: 'container',
  publishedAt: '2026-09-02T00:00:00.000Z',
  publishedBy: 'teacher-1',
}
const workRelease = { ...experimentRelease, id: 'release-work', purpose: 'work', name: 'Python 分析' }
const withdrawnRelease = { ...experimentRelease, id: 'release-withdrawn', withdrawal: { reasonCode: 'TEACHER_WITHDRAWN' } }

function resourceRequest(id: string, overrides: Record<string, unknown> = {}) {
  return {
    id,
    projectId: 'project-1',
    courseId: 'course-1',
    requesterId: 'student-1',
    requestKey: `work-env-${id}`,
    state: 'active',
    generation: 1,
    revision: 1,
    requestedDurationSeconds: 8 * 3600,
    requestedResources: { cpuMillicores: 2000, memoryBytes: 4 * 1024 ** 3, storageBytes: 20 * 1024 ** 3 },
    target: { kind: 'environment', environmentId: `env-${id}`, releaseId: 'release-exp', releaseVersion: 1 },
    diagnosticCode: null,
    createdAt: '2026-09-03T00:00:00.000Z',
    updatedAt: '2026-09-03T00:00:00.000Z',
    ...overrides,
  }
}

function mockRequests(items: unknown[]) {
  vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: { items, nextCursor: null } as never, error: undefined as never })
}

const accepted = (environmentId: string) => ({
  data: { environmentId, operationId: `op-${environmentId}`, revision: 1, statusUrl: `/operations/op-${environmentId}` },
  error: undefined as never,
}) as never

const ATTACH_STORAGE_KEY = 'labweaver.pending-command:student-1:environment-attach:request-1'

const projectsState = reactive({
  projects: { kind: 'success' as const, data: [projectOne, projectTwo] },
  selectedProjectId: projectOne.id as string | null,
  selectedProject: projectOne as typeof projectOne | null,
  select(id: string) {
    projectsState.selectedProjectId = id
    projectsState.selectedProject = projectsState.projects.data.find((project) => project.id === id) ?? null
  },
})

vi.mock('@/composables/useProjects', () => ({
  useProjects: () => projectsState,
}))

function environment(id: string, overrides: Record<string, unknown> = {}) {
  return {
    id,
    projectId: 'project-1',
    courseId: 'course-1',
    class: 'experiment',
    displayLabel: id,
    desiredState: 'running',
    eligibilityExpiresAt: '2026-09-20T00:00:00.000Z',
    endpoints: [],
    observedState: 'ready',
    ownerId: 'student-1',
    providerBinding: 'static',
    releaseId: 'release-1',
    releaseVersion: 1,
    revision: 1,
    runtimeKind: 'container',
    generation: 1,
    observedGeneration: 1,
    currentOperation: null,
    ...overrides,
  }
}

const mounted: Array<{ unmount: () => void }> = []

async function mountAt(projectId: string, props: Record<string, unknown> = {}) {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/student/labs', component: MyLabsView },
      { path: '/student/environments', component: { template: '<div />' } },
      { path: '/teacher/environments', component: { template: '<div />' } },
      { path: '/researcher/resources', component: { template: '<div />' } },
    ],
  })
  await router.push({ path: '/student/labs', query: { projectId } })
  await router.isReady()
  const wrapper = mount(MyLabsView, { props, global: { plugins: [router] } })
  mounted.push(wrapper)
  return { wrapper, router }
}

function enterButton(wrapper: VueWrapper, requestId = 'request-1') {
  return wrapper.get(`[data-testid="approved-requests"] [data-request-id="${requestId}"] button`)
}

describe('MyLabsView', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    projectsState.projects = { kind: 'success', data: [projectOne, projectTwo] }
    projectsState.selectedProjectId = projectOne.id
    projectsState.selectedProject = projectOne
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [] } as never, error: undefined as never })
    vi.mocked(listEnvironments).mockResolvedValue({ data: { items: [] } as never, error: undefined as never })
    mockRequests([])
    vi.mocked(listProjectResourceLeases).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined as never } as never)
    window.localStorage.clear()
    window.sessionStorage.clear()
    useAuth().user.value = { expired: false, profile: { actor_id: 'student-1', roles: ['student'] } } as never
  })

  afterEach(() => {
    mounted.splice(0).forEach((wrapper) => wrapper.unmount())
  })

  it('uses the URL project context and keeps deleted console actions disabled', async () => {
    vi.mocked(listEnvironments).mockResolvedValue({
      data: {
        items: [
          environment('deleted-work', {
            class: 'work',
            displayLabel: 'Deleted Work',
            desiredState: 'deleted',
            observedState: 'deleted',
            courseId: null,
          }),
          environment('reclaiming-course', {
            displayLabel: 'Reclaiming course environment',
            desiredState: 'deleted',
            observedState: 'provisioning',
            currentOperation: { state: 'accepted' },
          }),
          environment('ready-course', { displayLabel: 'Ready course environment' }),
        ],
      } as never,
      error: undefined as never,
    })

    const { wrapper, router } = await mountAt('project-1')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Deleted Work'))

    expect(wrapper.text()).toContain('Work 项目环境')
    expect(wrapper.text()).toContain('课程实验环境')
    expect(wrapper.text()).toContain('已删除；请创建新的项目环境')
    expect(wrapper.text()).toContain('删除已请求/正在回收；请等待清理完成')

    const deletedConsole = wrapper.find('button[title*="已删除"]')
    expect(deletedConsole.exists()).toBe(true)
    expect((deletedConsole.element as HTMLButtonElement).disabled).toBe(true)
    expect(wrapper.findAll('button').some((button) => button.text().includes('控制台') && !(button.element as HTMLButtonElement).disabled)).toBe(true)
    expect(router.currentRoute.value.query.projectId).toBe('project-1')
    expect(vi.mocked(listEnvironments)).toHaveBeenCalledWith({ query: { projectId: 'project-1', courseId: 'course-1' } })
  })

  it('shows permanent material retention without inventing a deadline', async () => {
    vi.mocked(listEnvironments).mockResolvedValue({
      data: { items: [environment('permanent-material', { eligibilityExpiresAt: null })] } as never,
      error: undefined as never,
    })

    const { wrapper } = await mountAt('project-1')
    await vi.waitFor(() => expect(wrapper.text()).toContain('permanent-material'))

    expect(wrapper.text()).toContain('材料保留至')
    expect(wrapper.text()).toContain('不过期，直到明确撤回')
    expect(wrapper.text()).not.toContain('Invalid Date')

    const detailsButton = wrapper.findAll('button').find((button) => button.text() === '详情')
    expect(detailsButton).toBeDefined()
    await detailsButton!.trigger('click')
    await vi.waitFor(() => expect(document.body.querySelector('.inspect-properties')).not.toBeNull())
    expect(document.body.querySelector('.inspect-properties')?.textContent).toContain('不过期，直到明确撤回')
  })

  it('opens failed cleanup in the same project while keeping its console disabled', async () => {
    vi.mocked(listEnvironments).mockResolvedValue({
      data: { items: [environment('failed-cleanup', {
        desiredState: 'deleted', observedState: 'failed',
        currentOperation: { kind: 'expire', state: 'failed', retryEligible: true },
      })] } as never, error: undefined as never,
    })
    const { wrapper, router } = await mountAt('project-1')
    await vi.waitFor(() => expect(wrapper.text()).toContain('处理回收'))
    expect(wrapper.text()).toContain('资源释放尚未确认')
    expect((wrapper.find('button[aria-label="控制台不可用"]').element as HTMLButtonElement).disabled).toBe(true)
    await wrapper.findAll('button').find((button) => button.text() === '处理回收')!.trigger('click')
    await vi.waitFor(() => expect(router.currentRoute.value.path).toBe('/student/environments'))
    expect(router.currentRoute.value.query).toEqual({ environmentId: 'failed-cleanup', projectId: 'project-1' })
    wrapper.unmount()
  })

  it.each(['accepted', 'running', 'cancelling', 'succeeded'])('does not offer cleanup recovery when the operation is %s', async (state) => {
    vi.mocked(listEnvironments).mockResolvedValue({
      data: { items: [environment('cleanup-pending', {
        desiredState: 'deleted', observedState: 'failed', currentOperation: { kind: 'expire', state },
      })] } as never, error: undefined as never,
    })
    const { wrapper } = await mountAt('project-1')
    await vi.waitFor(() => expect(wrapper.text()).toContain('cleanup-pending'))
    expect(wrapper.findAll('button').some((button) => button.text() === '处理回收')).toBe(false)
    wrapper.unmount()
  })

  it('does not fall back to another project while the requested project is loading', async () => {
    vi.mocked(listEnvironments).mockResolvedValue({
      data: { items: [environment('work-env', { projectId: 'project-2', courseId: null, class: 'work', displayLabel: 'Work environment' })] } as never,
      error: undefined as never,
    })
    projectsState.projects = { kind: 'loading', data: [] }
    projectsState.selectedProjectId = null
    projectsState.selectedProject = null
    const { wrapper } = await mountAt('project-2')

    expect(wrapper.text()).toContain('正在同步项目上下文，请稍候')
    expect(wrapper.text()).not.toContain('项目上下文未绑定')
    expect(vi.mocked(listEnvironments)).not.toHaveBeenCalled()

    projectsState.projects = { kind: 'success', data: [projectOne, projectTwo] }
    projectsState.selectedProjectId = 'project-2'
    projectsState.selectedProject = projectTwo
    await vi.waitFor(() => expect(vi.mocked(listEnvironments)).toHaveBeenCalledWith({ query: { projectId: 'project-2' } }))
    expect(wrapper.text()).toContain('Work 项目环境')
  })

  it('lets teachers inspect every environment in the project and opens the shared teacher console', async () => {
    vi.mocked(listEnvironments)
      .mockResolvedValueOnce({
        data: {
          items: [environment('experiment-env', { displayLabel: '实验环境' })],
          nextCursor: 'next-page',
        } as never,
        error: undefined as never,
      })
      .mockResolvedValueOnce({
        data: {
          items: [environment('work-env', {
            class: 'work',
            courseId: null,
            displayLabel: 'Work 环境',
          })],
          nextCursor: null,
        } as never,
        error: undefined as never,
      })

    const { wrapper, router } = await mountAt('project-1', {
      teacherMode: true,
      environmentPath: '/teacher/environments',
    })
    await vi.waitFor(() => expect(wrapper.text()).toContain('实验环境'))

    expect(vi.mocked(listEnvironments)).toHaveBeenCalledWith({ query: { projectId: 'project-1' } })
    expect(vi.mocked(listEnvironments)).toHaveBeenCalledWith({ query: { projectId: 'project-1', cursor: 'next-page' } })
    expect(wrapper.text()).toContain('课程实验环境')
    expect(wrapper.text()).toContain('Work 环境')
    expect(wrapper.findAll('button').some((button) => button.text().includes('申请实验环境'))).toBe(false)
    expect(wrapper.find('[data-testid="experiment-requests"]').exists()).toBe(false)
    expect(listProjectResourceRequests).not.toHaveBeenCalled()

    const consoleButton = wrapper.find('button[aria-label="打开控制台"]')
    await consoleButton.trigger('click')
    await vi.waitFor(() => expect(router.currentRoute.value.path).toBe('/teacher/environments'))
    expect(router.currentRoute.value.query).toEqual({ environmentId: 'experiment-env', projectId: 'project-1' })
  })

  it('sends the chosen experiment template to the resource request form instead of creating an environment', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [experimentRelease, workRelease, withdrawnRelease] } as never, error: undefined as never })
    const { wrapper, router } = await mountAt('project-1')
    await vi.waitFor(() => expect(wrapper.findAll('button').some((button) => button.text().includes('申请实验环境'))).toBe(true))
    await wrapper.findAll('button').find((button) => button.text().includes('申请实验环境'))!.trigger('click')

    // Only experiment versions that can still be requested are offered here.
    await vi.waitFor(() => expect(wrapper.findAll('.release-card').map((card) => card.attributes('data-release-id'))).toEqual(['release-exp']))
    expect(wrapper.get('.release-card').text()).toContain('缓冲区实验 v1')
    await wrapper.get('.release-card a').trigger('click')
    await vi.waitFor(() => expect(router.currentRoute.value.path).toBe('/researcher/resources'))
    expect(router.currentRoute.value.query).toEqual({ projectId: 'project-1', releaseId: 'release-exp', releaseVersion: '1' })
    expect(createEnvironment).not.toHaveBeenCalled()
  })

  it('tells a student without an approved request to submit one first', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [experimentRelease] } as never, error: undefined as never })
    const { wrapper } = await mountAt('project-1')
    const hint = await vi.waitFor(() => wrapper.get('[data-testid="approved-requests-empty"]'))
    expect(hint.text()).toContain('提交资源申请')
    expect(hint.text()).toContain('审批通过后')
    expect(wrapper.find('[data-testid="approved-requests"]').exists()).toBe(false)
    expect(createEnvironment).not.toHaveBeenCalled()
  })

  it('enters an approved environment by attaching to its resource request and opens the console', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [experimentRelease] } as never, error: undefined as never })
    mockRequests([resourceRequest('request-1')])
    vi.mocked(createEnvironment).mockResolvedValue(accepted('env-request-1'))
    const { wrapper, router } = await mountAt('project-1')
    await vi.waitFor(() => expect(enterButton(wrapper).text()).toBe('进入环境'))
    expect(wrapper.get('[data-testid="approved-requests"]').text()).toContain('缓冲区实验 v1')

    await enterButton(wrapper).trigger('click')
    await flushPromises()
    expect(createEnvironment).toHaveBeenCalledTimes(1)
    expect(vi.mocked(createEnvironment).mock.calls[0][0]).toEqual({
      headers: { 'Idempotency-Key': expect.any(String) },
      body: { projectId: 'project-1', courseId: 'course-1', resourceRequestId: 'request-1', releaseId: 'release-exp', releaseVersion: 1 },
    })
    await vi.waitFor(() => expect(router.currentRoute.value.path).toBe('/student/environments'))
    expect(router.currentRoute.value.query).toEqual({ environmentId: 'env-request-1', projectId: 'project-1' })
    // The accepted attach releases its key.
    expect(window.sessionStorage.getItem(ATTACH_STORAGE_KEY)).toBeNull()
  })

  it('replays the same attach after a lost response, a retry and a reload, so the request is attached once', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [experimentRelease] } as never, error: undefined as never })
    mockRequests([resourceRequest('request-1')])
    vi.mocked(createEnvironment)
      .mockRejectedValueOnce(new Error('request response unknown'))
      .mockRejectedValueOnce(new Error('request response unknown'))
      .mockResolvedValueOnce(accepted('env-request-1'))
    const first = await mountAt('project-1')
    await vi.waitFor(() => expect(enterButton(first.wrapper).exists()).toBe(true))
    await enterButton(first.wrapper).trigger('click')
    const row = () => first.wrapper.get('[data-testid="approved-requests"] [data-request-id="request-1"]')
    await vi.waitFor(() => expect(row().find('.diagnostic-banner').exists()).toBe(true))
    expect(row().text()).toContain('结果可能尚未确认')
    const key = vi.mocked(createEnvironment).mock.calls[0][0].headers['Idempotency-Key']
    expect(JSON.parse(window.sessionStorage.getItem(ATTACH_STORAGE_KEY)!).idempotencyKey).toBe(key)

    await row().get('.diagnostic-banner button').trigger('click')
    await vi.waitFor(() => expect(createEnvironment).toHaveBeenCalledTimes(2))
    expect(vi.mocked(createEnvironment).mock.calls[1][0]).toEqual(vi.mocked(createEnvironment).mock.calls[0][0])

    // A reload before any answer arrived replays the same command.
    first.wrapper.unmount()
    const reloaded = await mountAt('project-1')
    await vi.waitFor(() => expect(enterButton(reloaded.wrapper).exists()).toBe(true))
    await enterButton(reloaded.wrapper).trigger('click')
    await vi.waitFor(() => expect(createEnvironment).toHaveBeenCalledTimes(3))
    expect(vi.mocked(createEnvironment).mock.calls[2][0].headers['Idempotency-Key']).toBe(key)
    await vi.waitFor(() => expect(reloaded.router.currentRoute.value.query.environmentId).toBe('env-request-1'))
    expect(window.sessionStorage.getItem(ATTACH_STORAGE_KEY)).toBeNull()
  })

  it('shows a pending allocation as preparing, not as a failure, and enters again with the same key', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [experimentRelease] } as never, error: undefined as never })
    mockRequests([resourceRequest('request-1', { state: 'allocating' })])
    vi.mocked(createEnvironment)
      .mockResolvedValueOnce({
        data: undefined,
        error: { status: 409, diagnosticCode: 'LW_ENVIRONMENT_ALLOCATION_PENDING', detail: 'allocation pending', requestId: 'req-pending', retryable: false },
      } as never)
      .mockResolvedValueOnce(accepted('env-request-1'))
    const { wrapper, router } = await mountAt('project-1')
    await vi.waitFor(() => expect(enterButton(wrapper).exists()).toBe(true))
    await enterButton(wrapper).trigger('click')
    await flushPromises()

    const row = wrapper.get('[data-testid="approved-requests"] [data-request-id="request-1"]')
    expect(row.get('[data-testid="environment-preparing"]').text()).toContain('环境准备中')
    expect(row.find('.diagnostic-banner').exists()).toBe(false)
    expect((enterButton(wrapper).element as HTMLButtonElement).disabled).toBe(false)
    expect(router.currentRoute.value.path).toBe('/student/labs')

    await enterButton(wrapper).trigger('click')
    await vi.waitFor(() => expect(router.currentRoute.value.path).toBe('/student/environments'))
    const [firstCall, secondCall] = vi.mocked(createEnvironment).mock.calls
    expect(secondCall[0].headers['Idempotency-Key']).toBe(firstCall[0].headers['Idempotency-Key'])
    expect(window.sessionStorage.getItem(ATTACH_STORAGE_KEY)).toBeNull()
  })

  it('shows another refusal with its Chinese reason first and keeps the attach key', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [experimentRelease] } as never, error: undefined as never })
    mockRequests([resourceRequest('request-1')])
    vi.mocked(createEnvironment).mockResolvedValue({
      data: undefined,
      error: { status: 409, diagnosticCode: 'LW_RUNTIME_LEASE_NOT_ACTIVE', detail: 'lease not active', requestId: 'req-lease', retryable: false },
    } as never)
    const { wrapper, router } = await mountAt('project-1')
    await vi.waitFor(() => expect(enterButton(wrapper).exists()).toBe(true))
    await enterButton(wrapper).trigger('click')
    await flushPromises()

    const banner = wrapper.get('[data-testid="approved-requests"] [data-request-id="request-1"] .diagnostic-banner')
    expect(banner.text()).toContain('资源使用授权已到期、被回收或尚未生效')
    expect(banner.text().indexOf('资源使用授权')).toBeLessThan(banner.text().indexOf('LW_RUNTIME_LEASE_NOT_ACTIVE'))
    expect(wrapper.find('[data-testid="environment-preparing"]').exists()).toBe(false)
    expect(router.currentRoute.value.path).toBe('/student/labs')
    expect(JSON.parse(window.sessionStorage.getItem(ATTACH_STORAGE_KEY)!).idempotencyKey)
      .toBe(vi.mocked(createEnvironment).mock.calls[0][0].headers['Idempotency-Key'])
  })

  it('sends one attach when the entry is double-clicked while it is pending', async () => {
    let finish: ((value: unknown) => void) | undefined
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [experimentRelease] } as never, error: undefined as never })
    mockRequests([resourceRequest('request-1')])
    vi.mocked(createEnvironment).mockImplementation(() => new Promise((resolve) => { finish = resolve }) as never)
    const { wrapper } = await mountAt('project-1')
    await vi.waitFor(() => expect(enterButton(wrapper).exists()).toBe(true))
    await enterButton(wrapper).trigger('click')
    await enterButton(wrapper).trigger('click')
    await flushPromises()
    expect(createEnvironment).toHaveBeenCalledTimes(1)
    expect((enterButton(wrapper).element as HTMLButtonElement).disabled).toBe(true)
    finish!(accepted('env-request-1'))
    await flushPromises()
    expect(createEnvironment).toHaveBeenCalledTimes(1)
  })

  it('lists waiting and rejected requests and never offers an entry for an attached, foreign or Work request', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [experimentRelease, workRelease] } as never, error: undefined as never })
    vi.mocked(listEnvironments).mockResolvedValue({ data: { items: [environment('env-attached')] } as never, error: undefined as never })
    mockRequests([
      resourceRequest('request-waiting', { state: 'reviewing' }),
      resourceRequest('request-rejected', { state: 'rejected' }),
      resourceRequest('request-attached', { target: { kind: 'environment', environmentId: 'env-attached', releaseId: 'release-exp', releaseVersion: 1 } }),
      resourceRequest('request-foreign', { requesterId: 'student-2' }),
      resourceRequest('request-work', { target: { kind: 'environment', environmentId: 'env-work', releaseId: 'release-work', releaseVersion: 1 } }),
    ])
    const { wrapper } = await mountAt('project-1')
    const waiting = await vi.waitFor(() => wrapper.get('[data-testid="reviewing-requests"] [data-request-id="request-waiting"]'))
    expect(waiting.text()).toContain('等待管理员审批')
    expect(waiting.get('a').attributes('href')).toBe('/researcher/resources?projectId=project-1')

    const rejected = wrapper.get('[data-testid="rejected-requests"] [data-request-id="request-rejected"]')
    expect(rejected.text()).toContain('管理员拒绝了这次资源申请')
    expect(rejected.get('a').attributes('href')).toBe('/researcher/resources?projectId=project-1&releaseId=release-exp&releaseVersion=1')

    expect(wrapper.find('[data-testid="approved-requests"]').exists()).toBe(false)
    expect(wrapper.get('[data-testid="approved-requests-empty"]').exists()).toBe(true)
    expect(wrapper.text()).not.toContain('request-foreign')
    expect(createEnvironment).not.toHaveBeenCalled()
  })

  it('does not let a late accepted attach delete a newer command for the same request', async () => {
    let finish: ((value: unknown) => void) | undefined
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [experimentRelease] } as never, error: undefined as never })
    mockRequests([resourceRequest('request-1')])
    vi.mocked(createEnvironment).mockImplementation(() => new Promise((resolve) => { finish = resolve }) as never)
    const { wrapper } = await mountAt('project-1')
    await vi.waitFor(() => expect(enterButton(wrapper).exists()).toBe(true))
    await enterButton(wrapper).trigger('click')
    await flushPromises()
    // Another tab of the same student replaced the stored command.
    const stored = JSON.parse(window.sessionStorage.getItem(ATTACH_STORAGE_KEY)!)
    window.sessionStorage.setItem(ATTACH_STORAGE_KEY, JSON.stringify({ ...stored, idempotencyKey: 'key-b' }))
    finish!(accepted('env-request-1'))
    await flushPromises()
    expect(JSON.parse(window.sessionStorage.getItem(ATTACH_STORAGE_KEY)!).idempotencyKey).toBe('key-b')
  })

  it('warns instead of hiding approved requests when they cannot be read', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [experimentRelease] } as never, error: undefined as never })
    vi.mocked(listProjectResourceRequests).mockRejectedValue(new Error('network down'))
    const { wrapper } = await mountAt('project-1')
    const banner = await vi.waitFor(() => wrapper.get('[data-testid="experiment-requests"] .diagnostic-banner'))
    expect(banner.text()).toContain('无法读取实验资源申请')
  })
})
