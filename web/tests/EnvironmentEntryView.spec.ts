import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { h } from 'vue'
import { createRouter, createWebHistory, RouterView } from 'vue-router'
import EnvironmentEntryView from '@/views/student/EnvironmentEntryView.vue'
import GcpProjectSelector from '@/components/layout/GcpProjectSelector.vue'
import { useProjects } from '@/composables/useProjects'
import {
  listEnvironmentTemplateReleases,
  listProjects,
  listProjectResourceLeases,
  listProjectResourceRequests,
  getEnvironment,
  listEnvironmentEndpoints,
  listEnvironmentAccessGrants,
  getAccessGrant,
  listEnvironmentOperations,
  cancelEnvironmentOperation,
  startEnvironment,
  deleteEnvironment,
  retryEnvironment,
  restartEnvironment,
  freezeSubmission,
  getFrozenSubmission,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    listEnvironmentTemplateReleases: vi.fn(),
    listProjects: vi.fn(),
    listProjectResourceLeases: vi.fn(),
    listProjectResourceRequests: vi.fn(),
    createEnvironment: vi.fn(),
    getEnvironment: vi.fn(),
    listEnvironmentEndpoints: vi.fn(),
    listEnvironmentAccessGrants: vi.fn(),
    getAccessGrant: vi.fn(),
    listEnvironmentOperations: vi.fn(),
    cancelEnvironmentOperation: vi.fn(),
    startEnvironment: vi.fn(),
    freezeSubmission: vi.fn(),
    getFrozenSubmission: vi.fn(),
    stopEnvironment: vi.fn(),
    restartEnvironment: vi.fn(),
    deleteEnvironment: vi.fn(),
    retryEnvironment: vi.fn(),
  }
})

const mockRelease = {
  id: 'release-1',
  courseId: 'course-1',
  projectId: 'project-1',
  candidateId: 'candidate-1',
  candidateRevision: 2,
  environmentSpecSha256: 'a'.repeat(64),
  runtimeKind: 'container' as const,
  version: 1,
  publishedAt: '2026-07-11T10:00:00.000Z',
  publishedBy: 'teacher-1',
}

const mockSubmissionManifest = {
  apiVersion: 'evaluation.labweaver.io/v1' as const,
  kind: 'SubmissionManifest' as const,
  name: 'experiment-workspace',
  include: [{ kind: 'exactFile' as const, path: 'student/auth.c' }],
  exclude: [{ kind: 'directoryTree' as const, path: 'student/build' }],
  required: [{ kind: 'exactFile' as const, path: 'student/auth.c' }],
  llmReadable: [{ kind: 'exactFile' as const, path: 'student/feedback.md' }],
  followSymlinks: false,
  maxFiles: 100,
  maxTotalBytes: 1048576,
  source: 'workspace' as const,
}

const mockProject = {
  id: 'project-1',
  name: 'Course project',
  description: 'Project used by the environment tests',
  ownerActorId: 'teacher-1',
  courseId: 'course-1',
  state: 'active' as const,
  revision: 1,
  createdAt: '2026-07-11T10:00:00.000Z',
  updatedAt: '2026-07-11T10:00:00.000Z',
}

const mockProjectB = {
  ...mockProject,
  id: 'project-2',
  name: 'Second project',
}

type OperationFixtureState = 'accepted' | 'running' | 'cancelling' | 'succeeded' | 'failed' | 'cancelled'

function mockOperation(state: OperationFixtureState, overrides: Record<string, unknown> = {}) {
  const terminal = state === 'succeeded' || state === 'failed' || state === 'cancelled'
  const minuteByState: Record<OperationFixtureState, string> = {
    accepted: '01',
    running: '02',
    cancelling: '03',
    succeeded: '04',
    failed: '05',
    cancelled: '06',
  }
  return {
    environmentId: 'env-1',
    operationId: `op-${state}`,
    kind: 'start',
    state,
    acceptedRevision: 7,
    acceptedAt: `2026-07-11T10:${minuteByState[state]}:00.000Z`,
    deadlineAt: '2026-07-11T10:05:00.000Z',
    attempt: 2,
    maxAttempts: 3,
    retryEligible: state === 'failed',
    cancelEligible: state === 'accepted' || state === 'running' || state === 'cancelling',
    diagnosticCode: state === 'failed' ? 'ENVIRONMENT_START_FAILED' : null,
    traceId: `trace-${state}`,
    terminalAt: terminal ? '2026-07-11T10:06:00.000Z' : null,
    cleanupStartedAt: state === 'cancelling' ? '2026-07-11T10:04:00.000Z' : null,
    ...overrides,
  }
}

function mockEnvironmentInstance(overrides: Record<string, unknown> = {}) {
  vi.mocked(getEnvironment).mockResolvedValue({
    data: {
      id: 'env-1',
      projectId: 'project-1',
      courseId: 'course-1',
      class: 'experiment',
      displayLabel: 'Environment 1',
      desiredState: 'running',
      eligibilityExpiresAt: '2026-07-12T10:00:00.000Z',
      endpoints: [],
      observedState: 'ready',
      ownerId: 'student-1',
      providerBinding: 'static',
      releaseId: 'release-1',
      releaseVersion: 1,
      revision: 11,
      runtimeKind: 'container',
      generation: 1,
      observedGeneration: 1,
      operation: {
        id: 'op-current',
        acceptedAt: '2026-07-11T10:00:00.000Z',
        acceptedRevision: 11,
        actorId: 'student-1',
        attempt: 1,
        deadlineAt: '2026-07-11T10:05:00.000Z',
        kind: 'start',
        maxAttempts: 3,
        nextAttemptAt: '2026-07-11T10:00:00.000Z',
        preserveMutableDisk: false,
        providerStep: 0,
        state: 'running',
        traceId: 'trace-current',
      },
      ...overrides,
    },
    error: undefined as never,
  } as never)
  vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [] }, error: undefined as never })
}

async function mountAt(
  query: Record<string, string> = {},
  withProjectSelector = false,
  teacherMode = false,
  entryPath: '/student/environments' | '/researcher/environments' | '/teacher/environments' = teacherMode
    ? '/teacher/environments'
    : '/student/environments',
) {
  const router = createRouter({
    history: createWebHistory(),
    routes: [
      { path: '/student/environments', name: 'student-environments', component: EnvironmentEntryView },
      { path: '/researcher/environments', name: 'researcher-environments', component: EnvironmentEntryView },
      { path: '/teacher/environments', name: 'teacher-environments', component: EnvironmentEntryView, props: { teacherMode } },
    ],
  })
  await router.push({ path: entryPath, query })
  await router.isReady()
  const component = {
    setup: () => () => h('div', [
      ...(withProjectSelector ? [h(GcpProjectSelector)] : []),
      h(RouterView),
    ]),
  }
  const wrapper = mount(component, {
    global: { plugins: [router] },
  })
  mountedWrappers.push(wrapper)
  return { wrapper, router }
}

const mountedWrappers: Array<{ unmount: () => void }> = []

describe('EnvironmentEntryView', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject], error: undefined as never })
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [] }, error: undefined as never })
    vi.mocked(listEnvironmentAccessGrants).mockResolvedValue({ data: { items: [] }, error: undefined as never } as never)
    vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [] }, error: undefined as never } as never)
    vi.mocked(listEnvironmentOperations).mockResolvedValue({ data: { items: [] }, error: undefined as never })
    vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: [], error: undefined as never })
    vi.mocked(listProjectResourceLeases).mockResolvedValue({ data: [], error: undefined as never })
    window.localStorage.clear()
  })

  afterEach(() => {
    mountedWrappers.splice(0).forEach((wrapper) => wrapper.unmount())
    vi.unstubAllEnvs()
  })

  it('shows blocked diagnostic when no project context is available', async () => {
    vi.mocked(listProjects).mockResolvedValue({
      data: [],
      error: undefined as never,
    })
    const { wrapper } = await mountAt()
    await vi.waitFor(() => expect(wrapper.text()).toContain('PROJECT_CONTEXT_MISSING'))
  })

  it('keeps a task navigation when the project list resolves during the auth guard', async () => {
    let resolveProjects!: (value: unknown) => void
    const projectsResponse = new Promise((resolve) => { resolveProjects = resolve })
    vi.mocked(listProjects).mockImplementation(() => projectsResponse as never)

    let releaseGuard!: () => void
    const guardReady = new Promise<void>((resolve) => { releaseGuard = resolve })
    const RootView = {
      setup() {
        const projects = useProjects()
        projects.projects = { kind: 'idle' }
        projects.selectedProjectId = null
        return { projects }
      },
      template: '<RouterLink to="/student/environments" data-testid="student-environment-link">环境控制台</RouterLink>',
    }
    const router = createRouter({
      history: createWebHistory(),
      routes: [
        { path: '/', component: RootView },
        { path: '/student/environments', component: EnvironmentEntryView },
      ],
    })
    router.beforeEach(async (to) => {
      if (to.path === '/student/environments') await guardReady
    })
    await router.push('/')
    await router.isReady()
    const wrapper = mount({ setup: () => () => h(RouterView) }, {
      global: { plugins: [router] },
    })
    mountedWrappers.push(wrapper)
    await vi.waitFor(() => expect(vi.mocked(listProjects)).toHaveBeenCalledTimes(1))

    const navigation = router.push('/student/environments')
    await Promise.resolve()
    expect(router.currentRoute.value.path).toBe('/')

    resolveProjects({ data: [mockProject], error: undefined as never })
    await Promise.resolve()
    releaseGuard()
    await navigation
    await flushPromises()

    expect(router.currentRoute.value.path).toBe('/student/environments')
    await vi.waitFor(() => expect(wrapper.text()).toContain('项目环境控制台'))
    expect(router.currentRoute.value.query.projectId).toBe('project-1')
  })

  it('offers the project Work environment list before the advanced ID input', async () => {
    const { wrapper } = await mountAt(
      { projectId: 'project-1' },
      false,
      false,
      '/researcher/environments',
    )

    await vi.waitFor(() => expect(wrapper.text()).toContain('选择已有 Work 环境'))
    const listLink = wrapper.get('a[href="/researcher/workspaces?projectId=project-1"]')
    expect(listLink.text()).toContain('选择已有 Work 环境')
    expect(wrapper.get('.environment-id-input-details').attributes('open')).toBeUndefined()
    expect(wrapper.get('.environment-id-input-details summary').text()).toContain('高级')
  })

  it('loads environment template releases for the selected project', async () => {
    vi.mocked(listProjects).mockResolvedValue({
      data: [mockProject],
      error: undefined as never,
    })
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [mockRelease] },
      error: undefined as never,
    })
    const { wrapper } = await mountAt()
    await vi.waitFor(() => expect(wrapper.text()).toContain('release-1'))
    await vi.waitFor(() => expect(vi.mocked(listEnvironmentTemplateReleases)).toHaveBeenCalledWith({
      path: { projectId: 'project-1' },
      query: { limit: 100, courseId: 'course-1' },
    }))
  })
  it('loads environment console when environmentId query is present', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [] },
      error: undefined as never,
    })
    vi.mocked(getEnvironment).mockResolvedValue({
      data: {
        id: 'env-1',
        courseId: 'demo-course-1',
        class: 'experiment',
        displayLabel: 'Demo environment',
        desiredState: 'running',
        eligibilityExpiresAt: '2026-07-12T10:00:00.000Z',
        endpoints: [],
        observedState: 'ready',
        operation: {
          id: 'op-1',
          acceptedAt: '2026-07-11T10:00:00.000Z',
          acceptedRevision: 1,
          actorId: 'student-1',
          attempt: 1,
          deadlineAt: '2026-07-11T10:05:00.000Z',
          kind: 'create',
          maxAttempts: 3,
          nextAttemptAt: '2026-07-11T10:00:00.000Z',
          preserveMutableDisk: false,
          providerStep: 0,
          state: 'succeeded',
          traceId: 'trace-1',
        },
        ownerId: 'student-1',
        providerBinding: 'static',
        releaseId: 'release-1',
        releaseVersion: 1,
        revision: 1,
        runtimeKind: 'container',
      },
      error: undefined as never,
    })
    vi.mocked(listEnvironmentEndpoints).mockResolvedValue({
      data: { items: [{ id: 'ep-1', protocol: 'ssh', health: 'healthy', observedAt: '2026-07-11T10:00:00.000Z' }] },
      error: undefined as never,
    })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await vi.waitFor(() => expect(wrapper.text()).toContain('运行中'))
    expect(wrapper.text()).toContain('启动')
    expect(wrapper.get('#lifecycle-action-hint').text()).toContain('重启会中断当前运行')
    await vi.waitFor(() => expect(vi.mocked(listEnvironmentEndpoints)).toHaveBeenCalledWith({ path: { environmentId: 'env-1' } }))
    expect(wrapper.text()).toContain('ssh')
  })

  it('keeps a researcher Work console pending while the approved environment handoff appears', async () => {
    vi.useFakeTimers()
    try {
      mockEnvironmentInstance()
      vi.mocked(getEnvironment).mockResolvedValueOnce({
        response: { status: 404 },
        error: { diagnosticCode: 'LW_ENVIRONMENT_NOT_FOUND', detail: '环境尚未同步', retryable: true },
      } as never)
      vi.mocked(listProjectResourceRequests).mockResolvedValue({
        data: [{
          id: 'request-1',
          projectId: 'project-1',
          target: { kind: 'environment', environmentId: 'env-1', releaseId: 'release-1', releaseVersion: 1 },
          state: 'active',
        }],
        error: undefined as never,
      } as never)
      vi.mocked(listProjectResourceLeases).mockResolvedValue({
        data: [{ id: 'lease-1', requestId: 'request-1', state: 'active' }],
        error: undefined as never,
      } as never)
      const { wrapper } = await mountAt(
        { environmentId: 'env-1', projectId: 'project-1' },
        false,
        false,
        '/researcher/environments',
      )
      await flushPromises()
      expect(wrapper.text()).toContain('环境正在准备')

      await vi.advanceTimersByTimeAsync(3000)
      await flushPromises()

      expect(wrapper.text()).toContain('Environment 1')
      expect(getEnvironment).toHaveBeenCalledTimes(2)
    } finally {
      vi.useRealTimers()
    }
  })

  it('does not wait for an unknown Work environment ID', async () => {
    vi.mocked(getEnvironment).mockResolvedValue({
      response: { status: 404 },
      error: { diagnosticCode: 'LW_ENVIRONMENT_NOT_FOUND', detail: '环境不存在', retryable: true },
    } as never)
    const { wrapper } = await mountAt(
      { environmentId: 'env-unknown', projectId: 'project-1' },
      false,
      false,
      '/researcher/environments',
    )
    await flushPromises()
    expect(wrapper.text()).toContain('LW_ENVIRONMENT_NOT_FOUND')
    expect(wrapper.text()).not.toContain('环境正在准备')
    expect(listProjectResourceRequests).toHaveBeenCalledWith({ path: { projectId: 'project-1' } })
    expect(listProjectResourceLeases).toHaveBeenCalledWith({ path: { projectId: 'project-1' } })
    expect(getEnvironment).toHaveBeenCalledTimes(1)
  })

  it('does not wait when the matching Work request belongs to another project', async () => {
    vi.mocked(getEnvironment).mockResolvedValue({
      response: { status: 404 },
      error: { diagnosticCode: 'LW_ENVIRONMENT_NOT_FOUND', detail: '环境不存在', retryable: true },
    } as never)
    vi.mocked(listProjectResourceRequests).mockResolvedValue({
      data: [{
        id: 'request-other-project',
        projectId: 'project-2',
        target: { kind: 'environment', environmentId: 'env-1', releaseId: 'release-1', releaseVersion: 1 },
        state: 'active',
      }],
      error: undefined as never,
    } as never)
    vi.mocked(listProjectResourceLeases).mockResolvedValue({
      data: [{ id: 'lease-other-project', requestId: 'request-other-project', state: 'active' }],
      error: undefined as never,
    } as never)
    const { wrapper } = await mountAt(
      { environmentId: 'env-1', projectId: 'project-1' },
      false,
      false,
      '/researcher/environments',
    )
    await flushPromises()
    expect(wrapper.text()).toContain('LW_ENVIRONMENT_NOT_FOUND')
    expect(wrapper.text()).not.toContain('环境正在准备')
    expect(getEnvironment).toHaveBeenCalledTimes(1)
  })

  it('stops immediately with the resource diagnostic after an approved request becomes terminal', async () => {
    vi.mocked(getEnvironment).mockResolvedValue({
      response: { status: 404 },
      error: { diagnosticCode: 'LW_ENVIRONMENT_NOT_FOUND', detail: '环境尚未同步', retryable: true },
    } as never)
    vi.mocked(listProjectResourceRequests).mockResolvedValue({
      data: [{
        id: 'request-1',
        projectId: 'project-1',
        diagnosticCode: 'LW_ENVIRONMENT_CREATE_AGGREGATE_INVALID',
        target: { kind: 'environment', environmentId: 'env-1', releaseId: 'release-1', releaseVersion: 1 },
        state: 'rejected',
      }],
      error: undefined as never,
    } as never)
    vi.mocked(listProjectResourceLeases).mockResolvedValue({ data: [], error: undefined as never })
    const { wrapper } = await mountAt(
      { environmentId: 'env-1', projectId: 'project-1' },
      false,
      false,
      '/researcher/environments',
    )
    await flushPromises()
    expect(wrapper.text()).toContain('LW_ENVIRONMENT_CREATE_AGGREGATE_INVALID')
    expect(wrapper.text()).toContain('资源申请已拒绝')
    expect(wrapper.text()).not.toContain('环境正在准备')
    expect(getEnvironment).toHaveBeenCalledTimes(1)
  })

  it('stops immediately when the approved Work lease is terminal', async () => {
    vi.mocked(getEnvironment).mockResolvedValue({
      response: { status: 404 },
      error: { diagnosticCode: 'LW_ENVIRONMENT_NOT_FOUND', detail: '环境尚未同步', retryable: true },
    } as never)
    vi.mocked(listProjectResourceRequests).mockResolvedValue({
      data: [{
        id: 'request-1',
        projectId: 'project-1',
        target: { kind: 'environment', environmentId: 'env-1', releaseId: 'release-1', releaseVersion: 1 },
        state: 'active',
      }],
      error: undefined as never,
    } as never)
    vi.mocked(listProjectResourceLeases).mockResolvedValue({
      data: [{ id: 'lease-1', requestId: 'request-1', state: 'revoked', revokeReasonCode: 'task_owner_release' }],
      error: undefined as never,
    } as never)
    const { wrapper } = await mountAt(
      { environmentId: 'env-1', projectId: 'project-1' },
      false,
      false,
      '/researcher/environments',
    )
    await flushPromises()
    expect(wrapper.text()).toContain('task_owner_release')
    expect(wrapper.text()).toContain('资源授权已撤销')
    expect(wrapper.text()).not.toContain('环境正在准备')
    expect(getEnvironment).toHaveBeenCalledTimes(1)
  })

  it('surfaces a resource permission error instead of waiting on the environment 404', async () => {
    vi.mocked(getEnvironment).mockResolvedValue({
      response: { status: 404 },
      error: { diagnosticCode: 'LW_ENVIRONMENT_NOT_FOUND', detail: '环境尚未同步', retryable: true },
    } as never)
    vi.mocked(listProjectResourceRequests).mockResolvedValue({
      response: { status: 403 },
      error: { diagnosticCode: 'LW_AUTH_SCOPE_DENIED', detail: '无权读取项目资源', retryable: false },
    } as never)
    const { wrapper } = await mountAt(
      { environmentId: 'env-1', projectId: 'project-1' },
      false,
      false,
      '/researcher/environments',
    )
    await flushPromises()
    expect(wrapper.text()).toContain('LW_AUTH_SCOPE_DENIED')
    expect(wrapper.text()).toContain('无权读取项目资源')
    expect(wrapper.text()).not.toContain('环境正在准备')
    expect(getEnvironment).toHaveBeenCalledTimes(1)
  })

  it('keeps a direct student environment 404 as an error', async () => {
    vi.mocked(getEnvironment).mockResolvedValue({
      response: { status: 404 },
      error: { diagnosticCode: 'LW_ENVIRONMENT_NOT_FOUND', detail: '环境不存在', retryable: true },
    } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' })
    await flushPromises()
    expect(wrapper.text()).toContain('LW_ENVIRONMENT_NOT_FOUND')
    expect(getEnvironment).toHaveBeenCalledTimes(1)
  })

  it('explains retained storage and resource reservations while stopped', async () => {
    mockEnvironmentInstance({
      desiredState: 'stopped',
      observedState: 'stopped',
      operation: { ...mockOperation('succeeded', { kind: 'stop' }) },
    })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.get('#lifecycle-action-hint').text()).toContain('环境已停止'))
    const hint = wrapper.get('#lifecycle-action-hint').text()
    expect(hint).toContain('计算用量已停止计量')
    expect(hint).toContain('工作目录和磁盘仍保留并继续按存储费率核算')
    expect(hint).toContain('GPU 预留和 Work 资源租约会保留')
    expect(hint).toContain('删除环境并完成回收后才归还容量')

    await wrapper.find('button[aria-label="删除"]').trigger('click')
    const dialog = wrapper.findComponent({ name: 'ConfirmDialog' })
    expect(dialog.props('description')).toContain('工作目录及关联容器存储或虚拟机磁盘')
    expect(dialog.props('description')).toContain('操作不可恢复')
    expect(dialog.props('description')).toContain('项目材料、已冻结提交和评测记录不在本环境删除范围内')
  })

  it('keeps teacher console navigation in the teacher workbench and omits student submission controls', async () => {
    mockEnvironmentInstance({ displayLabel: '教师可管理环境' })
    const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, true)

    await vi.waitFor(() => expect(wrapper.text()).toContain('教师可管理环境'))
    expect(wrapper.text()).toContain('教师项目环境')
    expect(wrapper.findAll('button').some((button) => button.text().includes('实验提交与凭据'))).toBe(false)
    expect(wrapper.find('.freeze-section').exists()).toBe(false)
    const backLink = wrapper.find('a[href^="/teacher/environments"]')
    expect(backLink.exists()).toBe(true)
    expect(backLink.attributes('href')).toContain('projectId=project-1')
  })

  it('keeps teacher freeze recovery in the operations timeline without exposing freeze controls', async () => {
    mockEnvironmentInstance({ displayLabel: '教师可管理环境' })
    vi.mocked(listEnvironmentOperations).mockResolvedValue({
      data: { items: [mockOperation('running', { kind: 'freeze', operationId: 'freeze-running' })] },
      error: undefined as never,
    } as never)

    const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, true)
    await vi.waitFor(() => expect(wrapper.text()).toContain('教师可管理环境'))

    await wrapper.findAll('button').find((button) => button.text().includes('Web 控制台'))!.trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('冻结提交处理中，终端已暂时断开'))
    expect(wrapper.find('.freeze-section').exists()).toBe(false)
    expect(wrapper.text()).toContain('查看操作状态')
    expect(wrapper.text()).not.toContain('查看提交状态')

    await wrapper.findAll('button').find((button) => button.text() === '查看操作状态')!.trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('操作与诊断时间线'))
    expect(wrapper.find('.freeze-section').exists()).toBe(false)
  })

  it('uses the environment display name and keeps the full ID in secondary details', async () => {
    mockEnvironmentInstance()
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.find('.title-with-pill h2').text()).toBe('Environment 1'))
    expect(wrapper.find('.breadcrumb-current').text()).toBe('Environment 1')
    expect(wrapper.find('.title-with-pill h2').text()).not.toContain('env-1')
    expect(wrapper.find('.breadcrumb-current').text()).not.toContain('env-1')
    expect(wrapper.get('.environment-id-details').text()).toContain('env-1')
    expect(wrapper.get('.environment-id-details code').text()).toBe('env-1')
    expect(wrapper.get('.environment-id-details').text()).toContain('rev-11')
    expect(wrapper.get('.env-meta-grid').text()).not.toContain('修订版本')
    expect(wrapper.findAll('.resource-title-row > button')).toHaveLength(1)

    const toolsToggle = wrapper.find('.resource-title-row > button')
    expect(toolsToggle.attributes('aria-expanded')).toBe('false')
    expect(wrapper.find('.environment-selector').exists()).toBe(false)
    await toolsToggle.trigger('click')
    expect(toolsToggle.attributes('aria-expanded')).toBe('true')
    expect(wrapper.find('.environment-selector').exists()).toBe(true)
    expect(wrapper.text()).toContain('收起创建与切换')

    await toolsToggle.trigger('click')
    expect(toolsToggle.attributes('aria-expanded')).toBe('false')
    expect(wrapper.find('.environment-selector').exists()).toBe(false)
  })

  it('disables console lifecycle actions after the server reports deletion', async () => {
    mockEnvironmentInstance({ desiredState: 'deleted', observedState: 'deleted' })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    const lifecycleButtons = wrapper.find('.gcp-action-bar').findAll('button').filter((button) => (
      ['启动', '停止', '重启', '删除'].includes(button.text())
    ))
    expect(lifecycleButtons).toHaveLength(4)
    expect(lifecycleButtons.every((button) => (button.element as HTMLButtonElement).disabled)).toBe(true)
    expect(wrapper.text()).toContain('此项目环境已删除')
    expect(wrapper.find('.environment-selector').exists()).toBe(false)
    expect(wrapper.find('button[aria-expanded="false"]').exists()).toBe(true)
  })

  it.each(['experiment', 'work'])('reclaims failed cleanup with DELETE for a %s environment', async (environmentClass) => {
    mockEnvironmentInstance({
      class: environmentClass, desiredState: 'deleted', observedState: 'failed', failedPhase: 'expiring',
      operation: { ...mockOperation('failed', { kind: 'expire' }), id: 'op-failed' },
    })
    vi.mocked(listEnvironmentOperations).mockResolvedValue({
      data: { items: [mockOperation('failed', { kind: 'expire' })] }, error: undefined as never,
    } as never)
    let finishDelete!: (value: never) => void
    vi.mocked(deleteEnvironment).mockImplementation(() => new Promise((resolve) => { finishDelete = resolve }))
    const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' })
    await vi.waitFor(() => expect(wrapper.find('button[aria-label="重试回收"]').exists()).toBe(true))
    const reclaim = wrapper.find('button[aria-label="重试回收"]')
    expect((reclaim.element as HTMLButtonElement).disabled).toBe(false)
    expect(wrapper.text()).toContain('资源释放尚未确认')
    expect(wrapper.findAll('button').some((button) => button.text() === '重试失败的操作')).toBe(false)
    for (const action of ['启动', '重启']) expect((wrapper.find(`button[aria-label="${action}"]`).element as HTMLButtonElement).disabled).toBe(true)
    await reclaim.trigger('click')
    expect(wrapper.findComponent({ name: 'ConfirmDialog' }).props('description')).toContain('删除仍存在的工作目录及关联容器存储或虚拟机磁盘')
    wrapper.findComponent({ name: 'ConfirmDialog' }).vm.$emit('confirm')
    await vi.waitFor(() => expect(deleteEnvironment).toHaveBeenCalledTimes(1))
    expect(deleteEnvironment).toHaveBeenCalledWith({
      path: { environmentId: 'env-1' },
      headers: { 'If-Match': '"rev-11"', 'Idempotency-Key': expect.any(String) },
    })
    expect((reclaim.element as HTMLButtonElement).disabled).toBe(true)
    expect(retryEnvironment).not.toHaveBeenCalled()
    expect(startEnvironment).not.toHaveBeenCalled()
    expect(restartEnvironment).not.toHaveBeenCalled()
    finishDelete({ data: { environmentId: 'env-1', operationId: 'new-delete', revision: 12, statusUrl: '/api/v1/environments/env-1' }, error: undefined } as never)
    await vi.waitFor(() => expect(getEnvironment).toHaveBeenCalledTimes(2))
    // The accepted revision fences another delete while the instance read is still stale.
    expect((reclaim.element as HTMLButtonElement).disabled).toBe(true)
  })

  it.each(['accepted', 'running', 'cancelling'])('does not duplicate cleanup while the operation is %s', async (state) => {
    mockEnvironmentInstance({
      desiredState: 'deleted', observedState: 'failed',
      operation: { ...mockOperation(state as OperationFixtureState, { kind: 'expire' }), id: `op-${state}` },
    })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.find('button[aria-label="删除"]').exists()).toBe(true))
    expect((wrapper.find('button[aria-label="删除"]').element as HTMLButtonElement).disabled).toBe(true)
    expect(wrapper.find('button[aria-label="重试回收"]').exists()).toBe(false)
    expect(deleteEnvironment).not.toHaveBeenCalled()
  })

  it('does not expose recovery actions when environment ownership is denied', async () => {
    vi.mocked(getEnvironment).mockResolvedValue({ error: { diagnosticCode: 'LW_SCOPE_DENIED', detail: '无权访问此环境', status: 403 } } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('LW_SCOPE_DENIED'))
    expect(wrapper.find('button[aria-label="重试回收"]').exists()).toBe(false)
    expect(wrapper.find('button[aria-label="删除"]').exists()).toBe(false)
    expect(deleteEnvironment).not.toHaveBeenCalled()
  })

  it('shows a denied DELETE and releases the local pending state without pretending cleanup completed', async () => {
    mockEnvironmentInstance({
      desiredState: 'deleted', observedState: 'failed',
      operation: { ...mockOperation('failed', { kind: 'delete' }), id: 'op-failed' },
    })
    vi.mocked(deleteEnvironment).mockResolvedValue({ error: { diagnosticCode: 'LW_SCOPE_DENIED', detail: '回收权限已撤销', status: 403 } } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.find('button[aria-label="重试回收"]').exists()).toBe(true))
    await wrapper.find('button[aria-label="重试回收"]').trigger('click')
    wrapper.findComponent({ name: 'ConfirmDialog' }).vm.$emit('confirm')
    await vi.waitFor(() => expect(wrapper.text()).toContain('回收权限已撤销'))
    expect(wrapper.findAll('button').some((button) => button.text() === '重试')).toBe(false)
    expect(wrapper.text()).toContain('资源释放尚未确认')
    expect((wrapper.find('button[aria-label="重试回收"]').element as HTMLButtonElement).disabled).toBe(false)
    expect(deleteEnvironment).toHaveBeenCalledTimes(1)
    const firstKey = vi.mocked(deleteEnvironment).mock.calls[0][0]?.headers?.['Idempotency-Key']
    await wrapper.find('button[aria-label="重试回收"]').trigger('click')
    wrapper.findComponent({ name: 'ConfirmDialog' }).vm.$emit('confirm')
    await vi.waitFor(() => expect(deleteEnvironment).toHaveBeenCalledTimes(2))
    expect(vi.mocked(deleteEnvironment).mock.calls[1][0]?.headers?.['Idempotency-Key']).not.toBe(firstKey)
  })

  it('renders every public operation state and its optional cleanup and diagnostic details', async () => {
    mockEnvironmentInstance({ observedState: 'failed' })
    const operationItems = (['accepted', 'running', 'cancelling', 'failed', 'cancelled'] as const).map((state) =>
      mockOperation(state),
    )
    vi.mocked(listEnvironmentOperations).mockResolvedValue({
      data: { items: operationItems },
      error: undefined as never,
    } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    const operationsTab = wrapper.findAll('button').find((button) => button.text().includes('异步操作与诊断'))
    expect(operationsTab).toBeDefined()
    await operationsTab!.trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('操作与诊断时间线'))

    const text = wrapper.text()
    expect(text).toContain('已受理')
    expect(text).toContain('处理中')
    expect(text).toContain('取消中')
    expect(text).toContain('失败')
    expect(text).toContain('已取消')
    expect(text).toContain('资源清理已于')
    expect(text).toContain('诊断码：ENVIRONMENT_START_FAILED')
    expect(wrapper.findAll('button').some((button) => button.text() === '重试失败的操作')).toBe(true)
  })

  it('keeps the toolbar retry as the only retry entry for a failed environment', async () => {
    mockEnvironmentInstance({ observedState: 'failed' })
    vi.mocked(listEnvironmentOperations).mockResolvedValue({
      data: { items: [mockOperation('failed')] },
      error: undefined as never,
    } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    const terminalTab = wrapper.findAll('button').find((button) => button.text().includes('Web 控制台'))
    await terminalTab!.trigger('click')

    expect(wrapper.findAll('button').filter((button) => button.text() === '重试失败的操作')).toHaveLength(1)
    expect(wrapper.findAll('button').filter((button) => button.text() === '重试')).toHaveLength(0)
  })

  it.each([
    ['ready', 'running', true, '', '运行中'],
    ['stopped', 'stopped', false, '环境已停止，启动后才能签发访问授权。', '已停止'],
    ['failed', 'running', false, '环境处于失败状态，重试成功并恢复就绪后才能签发访问授权。', '运行中'],
  ] as const)('only offers access grants for a ready environment (%s)', async (observedState, desiredState, canIssue, hint, desiredLabel) => {
    mockEnvironmentInstance({ observedState, desiredState })
    vi.mocked(listEnvironmentEndpoints).mockResolvedValue({
      data: {
        items: [
          { id: 'ep-http', protocol: 'https', health: 'healthy', observedAt: '2026-07-11T10:00:00.000Z' },
          { id: 'ep-ssh', protocol: 'ssh', health: 'healthy', observedAt: '2026-07-11T10:00:00.000Z' },
        ],
      },
      error: undefined as never,
    } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await vi.waitFor(() => expect(wrapper.find('.access-section').text()).toContain(canIssue ? '签发访问授权' : hint))
    expect(wrapper.find('.env-meta-grid').text()).toContain(desiredLabel)
    const grantButton = wrapper.findAll('button').find((button) => button.text() === '签发访问授权')
    expect(Boolean(grantButton?.exists())).toBe(canIssue)
    if (!canIssue) expect(wrapper.text()).toContain(hint)
  })

  it('opens an HTTP endpoint through its authorized same-origin connect URL', async () => {
    mockEnvironmentInstance()
    vi.mocked(listEnvironmentEndpoints).mockResolvedValue({
      data: {
        items: [{ id: 'ep-http', protocol: 'http', health: 'healthy', observedAt: '2026-07-11T10:00:00.000Z' }],
      },
      error: undefined as never,
    } as never)
    vi.mocked(listEnvironmentAccessGrants).mockResolvedValue({
      data: { items: [{ id: 'grant-http' }] },
      error: undefined as never,
    } as never)
    vi.mocked(getAccessGrant).mockResolvedValue({
      data: {
        id: 'grant-http',
        actorId: 'student-1',
        environmentId: 'env-1',
        environmentRevision: 11,
        projectId: 'project-1',
        state: 'active',
        revision: 1,
        endpointGrants: [{
          id: 'endpoint-grant-http',
          accessGrantId: 'grant-http',
          endpointId: 'ep-http',
          endpointRevision: 1,
          protocol: 'http',
          action: 'connect',
          health: 'healthy',
          connectUrl: '/connect/endpoint-grant-http/',
          expiresAt: '2026-07-12T10:00:00.000Z',
        }],
        issuedAt: '2026-07-11T10:00:00.000Z',
        expiresAt: '2026-07-12T10:00:00.000Z',
      },
      error: undefined as never,
    } as never)
    const open = vi.spyOn(window, 'open').mockImplementation(() => null)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.find('.runtime-access').exists()).toBe(true))
    expect(wrapper.find('.endpoint-grants').text()).toContain('http')
    const openButton = wrapper.findAll('button').find((button) => button.text() === '打开容器实验')
    expect(openButton).toBeDefined()
    await openButton!.trigger('click')
    expect(open).toHaveBeenCalledWith('/connect/endpoint-grant-http/', '_blank', 'noopener,noreferrer')
    open.mockRestore()
  })

  it.each([
    ['stopped', 'stopped', '环境已停止，启动后才能签发访问授权。'],
    ['failed', 'running', '环境处于失败状态，重试成功并恢复就绪后才能签发访问授权。'],
  ] as const)('does not offer terminal access grant when the environment is not ready (%s)', async (observedState, desiredState, hint) => {
    mockEnvironmentInstance({ observedState, desiredState })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await wrapper.findAll('button').find((button) => button.text().includes('Web 控制台'))!.trigger('click')
    const pane = wrapper.get('.console-unauthorized-pane')
    expect(pane.text()).toContain(hint)
    expect(pane.text()).not.toContain('一键签发')
    expect(pane.find('button').exists()).toBe(false)
  })

  it('uses an explicit URL project over the shared context and preserves a linked environment', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject, mockProjectB], error: undefined as never })
    mockEnvironmentInstance()
    const { wrapper, router } = await mountAt({ environmentId: 'env-1' }, true)

    await vi.waitFor(() => expect(wrapper.find('.selector-trigger').text()).toContain('Course project'))
    await router.push({
      path: '/student/environments',
      query: { projectId: 'project-2', environmentId: 'env-2' },
    })

    await vi.waitFor(() => expect(wrapper.find('.selector-trigger').text()).toContain('Second project'))
    expect(wrapper.find('.selector-trigger').text()).not.toContain('project-2')
    expect(router.currentRoute.value.query.environmentId).toBe('env-2')
  })

  it('clears the environment and opens its tools when the shared project changes', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject, mockProjectB], error: undefined as never })
    mockEnvironmentInstance()
    const { wrapper, router } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, true)

    await vi.waitFor(() => expect(wrapper.find('.selector-trigger').text()).toContain('Course project'))
    await wrapper.get('.selector-trigger').trigger('click')
    await wrapper.findAll('.selector-menu .project-item').find((item) => item.text().includes('Second project'))!.trigger('click')

    await vi.waitFor(() => expect(router.currentRoute.value.query.projectId).toBe('project-2'))
    expect(router.currentRoute.value.query.environmentId).toBeUndefined()
    expect(wrapper.find('.placeholder-pane').exists()).toBe(true)
    expect(wrapper.find('.environment-selector').exists()).toBe(true)
  })

  it('cancels the active operation with the selected environment revision', async () => {
    mockEnvironmentInstance({ revision: 11 })
    vi.mocked(listEnvironmentOperations).mockResolvedValue({
      data: { items: [mockOperation('running')] },
      error: undefined as never,
    } as never)
    vi.mocked(cancelEnvironmentOperation).mockResolvedValue({
      data: {
        environmentId: 'env-1',
        operationId: 'op-cancel',
        revision: 12,
        statusUrl: '/api/v1/operations/op-cancel',
      },
      error: undefined as never,
    } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    const operationsTab = wrapper.findAll('button').find((button) => button.text().includes('异步操作与诊断'))
    await operationsTab!.trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('取消操作'))
    await wrapper.findAll('button').find((button) => button.text() === '取消操作')!.trigger('click')

    await vi.waitFor(() => expect(cancelEnvironmentOperation).toHaveBeenCalled())
    expect(cancelEnvironmentOperation).toHaveBeenCalledWith({
      path: { environmentId: 'env-1' },
      headers: {
        'Idempotency-Key': expect.any(String),
        'If-Match': '"rev-11"',
      },
    })
  })

  it('shows a cancellation diagnostic when the cancel request fails', async () => {
    mockEnvironmentInstance()
    vi.mocked(listEnvironmentOperations).mockResolvedValue({
      data: { items: [mockOperation('running')] },
      error: undefined as never,
    } as never)
    vi.mocked(cancelEnvironmentOperation).mockResolvedValue({
      data: undefined,
      error: {
        diagnosticCode: 'OPERATION_CANCEL_CONFLICT',
        detail: '环境修订版本已变化',
        retryable: true,
      },
    } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    const operationsTab = wrapper.findAll('button').find((button) => button.text().includes('异步操作与诊断'))
    await operationsTab!.trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('取消操作'))
    await wrapper.findAll('button').find((button) => button.text() === '取消操作')!.trigger('click')

    await vi.waitFor(() => expect(wrapper.text()).toContain('OPERATION_CANCEL_CONFLICT'))
    expect(wrapper.text()).toContain('环境修订版本已变化')
  })

  it('surfaces operation history errors in the operations tab', async () => {
    mockEnvironmentInstance()
    vi.mocked(listEnvironmentOperations).mockResolvedValue({
      data: undefined,
      error: {
        diagnosticCode: 'OPERATION_LIST_UNAVAILABLE',
        detail: '操作历史暂时不可用',
        retryable: false,
      },
    } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    const operationsTab = wrapper.findAll('button').find((button) => button.text().includes('异步操作与诊断'))
    await operationsTab!.trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('OPERATION_LIST_UNAVAILABLE'))
    expect(wrapper.text()).toContain('操作历史暂时不可用')
  })

  it('fails closed for an operation state the client does not recognize', async () => {
    mockEnvironmentInstance()
    vi.mocked(listEnvironmentOperations).mockResolvedValue({
      data: { items: [mockOperation('running', { state: 'future_state', retryEligible: true, cancelEligible: true })] },
      error: undefined as never,
    } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    const operationsTab = wrapper.findAll('button').find((button) => button.text().includes('异步操作与诊断'))
    await operationsTab!.trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('状态未知'))

    expect(wrapper.text()).toContain('当前状态无法识别，请刷新页面。')
    expect(wrapper.findAll('button').some((button) => button.text() === '取消操作')).toBe(false)
    expect(wrapper.findAll('button').some((button) => button.text() === '重试失败的操作')).toBe(false)
  })

  it('shows lifecycle failure diagnostic to student', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [] },
      error: undefined as never,
    })
    vi.mocked(getEnvironment).mockResolvedValue({
      data: {
        id: 'env-1',
        courseId: 'demo-course-1',
        class: 'experiment',
        displayLabel: 'Demo environment',
        desiredState: 'stopped',
        eligibilityExpiresAt: '2026-07-12T10:00:00.000Z',
        endpoints: [],
        observedState: 'stopped',
        operation: {
          id: 'op-1',
          acceptedAt: '2026-07-11T10:00:00.000Z',
          acceptedRevision: 1,
          actorId: 'student-1',
          attempt: 1,
          deadlineAt: '2026-07-11T10:05:00.000Z',
          kind: 'create',
          maxAttempts: 3,
          nextAttemptAt: '2026-07-11T10:00:00.000Z',
          preserveMutableDisk: false,
          providerStep: 0,
          state: 'failed',
          traceId: 'trace-1',
        },
        ownerId: 'student-1',
        providerBinding: 'static',
        releaseId: 'release-1',
        releaseVersion: 1,
        revision: 2,
        runtimeKind: 'container',
      },
      error: undefined as never,
    })
    vi.mocked(listEnvironmentEndpoints).mockResolvedValue({
      data: { items: [] },
      error: undefined as never,
    })
    vi.mocked(startEnvironment).mockResolvedValue({
      data: undefined as never,
      error: {
        response: {
          data: {
            diagnosticCode: 'ENVIRONMENT_LIFECYCLE_FAILED',
            detail: '环境 env-1 处于失败状态，无法执行 start 操作',
            retryable: false,
          },
        },
      } as never,
    })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))

    await vi.waitFor(() => expect(wrapper.findAll('button').find((b) => b.text() === '启动')).toBeDefined())
    const startButton = wrapper.findAll('button').find((b) => b.text() === '启动')
    await startButton!.trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('ENVIRONMENT_LIFECYCLE_FAILED'))
    expect(wrapper.text()).toContain('环境 env-1 处于失败状态')
  })

  it('shows the freeze operation state and explains the temporary console disconnect', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject, mockProjectB], error: undefined as never })
    mockEnvironmentInstance({ projectId: 'project-2' })
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [{ ...mockRelease, projectId: 'project-2', submissionManifest: mockSubmissionManifest }] },
      error: undefined as never,
    } as never)
    vi.mocked(listEnvironmentOperations).mockResolvedValue({
      data: { items: [mockOperation('running', { kind: 'freeze', operationId: 'freeze-running' })] },
      error: undefined as never,
    } as never)

    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))

    await wrapper.findAll('button').find((button) => button.text().includes('实验提交与凭据'))!.trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('冻结中'))
    expect(wrapper.text()).toContain('不可变快照')
    const resultsLink = wrapper.find('a[href^="/student/results"]')
    expect(resultsLink.exists()).toBe(true)
    expect(resultsLink.attributes('href')).toContain('/student/results?projectId=project-2')

    await wrapper.findAll('button').find((button) => button.text().includes('Web 控制台'))!.trigger('click')
    expect(wrapper.text()).toContain('冻结提交处理中，终端已暂时断开')
    expect(wrapper.text()).toContain('查看提交状态')
  })

  it('uses the manifest from the exact environment release and submits it unchanged', async () => {
    mockEnvironmentInstance()
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: {
        items: [
          { ...mockRelease, version: 2, submissionManifest: { ...mockSubmissionManifest, name: 'wrong-version' } },
          { ...mockRelease, submissionManifest: mockSubmissionManifest },
        ],
      },
      error: undefined as never,
    } as never)
    vi.mocked(freezeSubmission).mockResolvedValue({
      data: {
        environmentId: 'env-1',
        operationId: 'freeze-op-1',
        revision: 12,
        statusUrl: '/api/v1/projects/project-1/frozen-submissions/00000000-0000-7000-8000-000000000001',
      },
      error: undefined as never,
    } as never)
    vi.mocked(getFrozenSubmission).mockResolvedValue({
      data: {
        object: {
          artifactId: 'artifact-1',
          mediaType: 'application/tar',
          objectVersion: 'object-version-1',
          sizeBytes: 123,
          storeBinding: 'evaluation-store',
        },
        contentSha256: 'b'.repeat(64),
        frozenAt: '2026-07-11T10:10:00.000Z',
      },
      error: undefined as never,
    } as never)

    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await wrapper.findAll('button').find((button) => button.text().includes('实验提交与凭据'))!.trigger('click')

    const startButton = wrapper.findAll('button').find((button) => button.text() === '发起冻结提交')!
    await vi.waitFor(() => expect(startButton.attributes('disabled')).toBeUndefined())
    await startButton.trigger('click')
    const dialog = wrapper.get('[role="dialog"]')
    expect(dialog.find('.freeze-manifest').text()).toContain('student/auth.c')
    expect(dialog.find('.freeze-manifest').text()).toContain('student/feedback.md')
    expect(dialog.find('.freeze-manifest').text()).not.toContain('wrong-version')

    await dialog.findAll('button').find((button) => button.text() === '确认冻结')!.trigger('click')
    await vi.waitFor(() => expect(vi.mocked(freezeSubmission)).toHaveBeenCalledTimes(1))
    await vi.waitFor(() => expect(wrapper.find('.evidence-card').exists()).toBe(true))

    await wrapper.findAll('button').find((button) => button.text().includes('Web 控制台'))!.trigger('click')
    expect(wrapper.text()).toContain('冻结完成，终端连接已断开')
    expect(wrapper.text()).toContain('重新签发授权并连接终端')

    const request = vi.mocked(freezeSubmission).mock.calls[0][0] as never as {
      path: { environmentId: string }
      headers: { 'Idempotency-Key': string; 'If-Match': string }
      body: { manifest: typeof mockSubmissionManifest }
    }
    expect(request.path).toEqual({ environmentId: 'env-1' })
    expect(request.headers['If-Match']).toBe('"rev-11"')
    expect(request.headers['Idempotency-Key']).toEqual(expect.any(String))
    expect(request.body.manifest).toEqual(mockSubmissionManifest)
  })

  it('blocks submission when the exact release has no manifest', async () => {
    mockEnvironmentInstance()
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: {
        items: [
          { ...mockRelease, version: 2, submissionManifest: mockSubmissionManifest },
          { ...mockRelease },
        ],
      },
      error: undefined as never,
    } as never)

    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await wrapper.findAll('button').find((button) => button.text().includes('实验提交与凭据'))!.trigger('click')

    await vi.waitFor(() => expect(wrapper.text()).toContain('此版本未配置提交评测'))
    const startButton = wrapper.findAll('button').find((button) => button.text() === '发起冻结提交')!
    expect(startButton.attributes('disabled')).toBeDefined()
    expect(freezeSubmission).not.toHaveBeenCalled()
  })

  it('blocks submission when the exact release has been withdrawn', async () => {
    mockEnvironmentInstance()
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: {
        items: [{
          ...mockRelease,
          submissionManifest: mockSubmissionManifest,
          withdrawal: {
            releaseId: 'release-1',
            releaseVersion: 1,
            actorId: 'teacher-1',
            reasonCode: 'teacher_withdrew',
            withdrawnAt: '2026-07-11T10:30:00.000Z',
          },
        }],
      },
      error: undefined as never,
    } as never)

    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await wrapper.findAll('button').find((button) => button.text().includes('实验提交与凭据'))!.trigger('click')

    await vi.waitFor(() => expect(wrapper.text()).toContain('当前环境所用版本已撤回'))
    const startButton = wrapper.findAll('button').find((button) => button.text() === '发起冻结提交')!
    expect(startButton.attributes('disabled')).toBeDefined()
    expect(freezeSubmission).not.toHaveBeenCalled()
  })

  it('reuses the freeze idempotency key when a retryable submission error is retried', async () => {
    mockEnvironmentInstance()
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [{ ...mockRelease, submissionManifest: mockSubmissionManifest }] },
      error: undefined as never,
    } as never)
    vi.mocked(freezeSubmission)
      .mockResolvedValueOnce({
        data: undefined as never,
        error: { diagnosticCode: 'FREEZE_TEMPORARY_FAILURE', detail: '暂时无法受理', retryable: true },
      } as never)
      .mockResolvedValueOnce({
        data: {
          environmentId: 'env-1',
          operationId: 'freeze-op-2',
          revision: 12,
        statusUrl: '/api/v1/projects/project-1/frozen-submissions/00000000-0000-7000-8000-000000000002',
        },
        error: undefined as never,
      } as never)
    vi.mocked(getFrozenSubmission).mockResolvedValue({
      data: {
        object: {
          artifactId: 'artifact-2',
          mediaType: 'application/tar',
          objectVersion: 'object-version-2',
          sizeBytes: 123,
          storeBinding: 'evaluation-store',
        },
        contentSha256: 'c'.repeat(64),
        frozenAt: '2026-07-11T10:11:00.000Z',
      },
      error: undefined as never,
    } as never)

    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await wrapper.findAll('button').find((button) => button.text().includes('实验提交与凭据'))!.trigger('click')
    await vi.waitFor(() => expect(wrapper.findAll('button').find((button) => button.text() === '发起冻结提交')?.attributes('disabled')).toBeUndefined())
    await wrapper.findAll('button').find((button) => button.text() === '发起冻结提交')!.trigger('click')
    await wrapper.get('[role="dialog"]').findAll('button').find((button) => button.text() === '确认冻结')!.trigger('click')

    await vi.waitFor(() => expect(wrapper.text()).toContain('FREEZE_TEMPORARY_FAILURE'))
    await wrapper.findAll('button').find((button) => button.text() === '重试')!.trigger('click')
    await vi.waitFor(() => expect(vi.mocked(freezeSubmission)).toHaveBeenCalledTimes(2))
    await vi.waitFor(() => expect(wrapper.find('.evidence-card').exists()).toBe(true))

    const firstRequest = vi.mocked(freezeSubmission).mock.calls[0][0] as never as { headers: { 'Idempotency-Key': string } }
    const secondRequest = vi.mocked(freezeSubmission).mock.calls[1][0] as never as { headers: { 'Idempotency-Key': string } }
    expect(secondRequest.headers['Idempotency-Key']).toBe(firstRequest.headers['Idempotency-Key'])
  })
})
