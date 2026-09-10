import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createWebHistory } from 'vue-router'
import EnvironmentEntryView from '@/views/student/EnvironmentEntryView.vue'
import {
  listEnvironmentTemplateReleases,
  listProjects,
  getEnvironment,
  listEnvironmentEndpoints,
  listEnvironmentOperations,
  cancelEnvironmentOperation,
  startEnvironment,
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
    listEnvironmentOperations: vi.fn(),
    cancelEnvironmentOperation: vi.fn(),
    startEnvironment: vi.fn(),
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

async function mountAt(query: Record<string, string> = {}) {
  const router = createRouter({
    history: createWebHistory(),
    routes: [{ path: '/student/environments', name: 'student-environments', component: EnvironmentEntryView }],
  })
  await router.push({ path: '/student/environments', query })
  await router.isReady()
  const wrapper = mount(EnvironmentEntryView, {
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
        desiredState: 'stopped',
        eligibilityExpiresAt: '2026-07-12T10:00:00.000Z',
        endpoints: [],
        observedState: 'failed',
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
})
