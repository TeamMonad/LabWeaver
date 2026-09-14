import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { defineComponent, h } from 'vue'
import { createRouter, createWebHistory, RouterView } from 'vue-router'
import EnvironmentEntryView from '@/views/student/EnvironmentEntryView.vue'
import GcpProjectSelector from '@/components/layout/GcpProjectSelector.vue'
import {
  listEnvironmentTemplateReleases,
  listProjects,
  getEnvironment,
  listEnvironmentEndpoints,
  listEnvironmentAccessGrants,
  listEnvironmentOperations,
  cancelEnvironmentOperation,
  startEnvironment,
  freezeSubmission,
  getFrozenSubmission,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    listEnvironmentTemplateReleases: vi.fn(),
    listProjects: vi.fn(),
    createEnvironment: vi.fn(),
    getEnvironment: vi.fn(),
    listEnvironmentEndpoints: vi.fn(),
    listEnvironmentAccessGrants: vi.fn(),
    listEnvironmentOperations: vi.fn(),
    cancelEnvironmentOperation: vi.fn(),
    startEnvironment: vi.fn(),
    freezeSubmission: vi.fn(),
    getFrozenSubmission: vi.fn(),
    stopEnvironment: vi.fn(),
    restartEnvironment: vi.fn(),
    deleteEnvironment: vi.fn(),
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

async function mountAt(query: Record<string, string> = {}, withProjectSelector = false) {
  const router = createRouter({
    history: createWebHistory(),
    routes: [{ path: '/student/environments', name: 'student-environments', component: EnvironmentEntryView }],
  })
  await router.push({ path: '/student/environments', query })
  await router.isReady()
  const component = defineComponent({
    setup: () => () => h('div', [
      ...(withProjectSelector ? [h(GcpProjectSelector)] : []),
      h(RouterView),
    ]),
  })
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
    vi.mocked(listEnvironmentOperations).mockResolvedValue({ data: { items: [] }, error: undefined as never })
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
    expect(wrapper.text()).toContain('运行中')
    expect(wrapper.text()).toContain('启动')
    await vi.waitFor(() => expect(vi.mocked(listEnvironmentEndpoints)).toHaveBeenCalledWith({ path: { environmentId: 'env-1' } }))
    expect(wrapper.text()).toContain('ssh')
  })

  it('uses the environment display name and keeps the full ID in secondary details', async () => {
    mockEnvironmentInstance()
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.find('.title-with-pill h2').text()).toBe('Environment 1'))
    expect(wrapper.find('.breadcrumb-current').text()).toBe('Environment 1')
    expect(wrapper.find('.title-with-pill h2').text()).not.toContain('env-1')
    expect(wrapper.find('.breadcrumb-current').text()).not.toContain('env-1')
    expect(wrapper.get('.environment-id-details').text()).toContain('env-1')
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
    expect(wrapper.find('.selector-trigger').text()).toContain('project-2')
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

    const startButton = wrapper.findAll('button').find((b) => b.text() === '启动')
    expect(startButton).toBeDefined()
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
