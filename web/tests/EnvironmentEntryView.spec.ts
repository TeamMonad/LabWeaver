import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { h } from 'vue'
import { createRouter, createWebHistory, RouterView } from 'vue-router'
import EnvironmentEntryView from '@/views/student/EnvironmentEntryView.vue'
import GcpProjectSelector from '@/components/layout/GcpProjectSelector.vue'
import { useProjects } from '@/composables/useProjects'
import { useAuth } from '@/composables/useAuth'
import {
  createEnvironment,
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
  stopEnvironment,
  freezeSubmission,
  getFrozenSubmission,
  listConsoleCapabilities,
  createAccessGrant,
  getOwnProjectEvaluationTaskForSubmission,
  getOwnProjectEvaluationResult,
  getProjectAvailableActions,
  upgradeEnvironmentTemplate,
} from '@/generated/contracts'
import { rememberSubmission } from '@/utils/trackedSubmissions'
import { makeDiagnostic } from '@/types/async'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getProjectAvailableActions: vi.fn(),
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
    getOwnProjectEvaluationTaskForSubmission: vi.fn(),
    getOwnProjectEvaluationResult: vi.fn(),
    stopEnvironment: vi.fn(),
    restartEnvironment: vi.fn(),
    deleteEnvironment: vi.fn(),
    retryEnvironment: vi.fn(),
    listConsoleCapabilities: vi.fn(),
    createAccessGrant: vi.fn(),
    upgradeEnvironmentTemplate: vi.fn(),
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
  // The server's connection read model; tests that care about it override `connection` directly.
  const instanceReady = (overrides.desiredState ?? 'running') === 'running' && (overrides.observedState ?? 'ready') === 'ready'
  vi.mocked(getEnvironment).mockResolvedValue({
    data: {
      connection: { instanceReady, state: instanceReady ? 'ready' : 'preparing', guestInstructions: [] },
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

function activeGrant(expiresAt: string) {
  return {
    id: 'grant-1',
    actorId: 'student-1',
    environmentId: 'env-1',
    environmentRevision: 11,
    projectId: 'project-1',
    state: 'active',
    revision: 1,
    endpointGrants: [],
    issuedAt: '2026-07-11T10:00:00.000Z',
    expiresAt,
  }
}

function consoleAvailability(kinds: Array<'xterm' | 'novnc'>) {
  return {
    accessGrantId: 'grant-1',
    accessGrantRevision: 1,
    environmentClass: 'experiment',
    environmentId: 'env-1',
    environmentRevision: 11,
    expiresAt: '2026-07-12T10:00:00.000Z',
    kinds,
    leaseFence: null,
    projectId: 'project-1',
  }
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

/** Signs a student in (or out with `null`) through the real auth state. */
function signIn(actorId: string | null) {
  useAuth().user.value = actorId ? { expired: false, profile: { actor_id: actorId, roles: ['student'] } } : null
}

/** Opens a detail tab the way a pointer does: Reka tabs activate on the primary-button press. */
async function openTab(wrapper: VueWrapper, label: string) {
  await wrapper.findAll('[role="tab"]').find((tab) => tab.text().includes(label))!.trigger('mousedown', { button: 0 })
}

describe('EnvironmentEntryView', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
    vi.mocked(getProjectAvailableActions).mockResolvedValue({
      data: {
        projectId: 'project-1',
        actorId: 'actor-1',
        membershipRevision: 1,
        validUntil: '2099-01-01T00:00:00.000Z',
        actions: ['browse', 'edit', 'start', 'stop', 'configure_agent', 'grant_access', 'manage_members', 'view_costs'],
      },
      error: undefined,
    } as never)
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject], error: undefined as never })
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [] }, error: undefined as never })
    vi.mocked(listEnvironmentAccessGrants).mockResolvedValue({ data: { items: [] }, error: undefined as never } as never)
    vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [] }, error: undefined as never } as never)
    vi.mocked(listEnvironmentOperations).mockResolvedValue({ data: { items: [] }, error: undefined as never })
    vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined as never })
    vi.mocked(listProjectResourceLeases).mockResolvedValue({ data: [], error: undefined as never })
    vi.mocked(listConsoleCapabilities).mockResolvedValue({ data: consoleAvailability(['xterm']), error: undefined as never } as never)
    vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue({
      data: undefined,
      error: { status: 404, diagnosticCode: 'LW_EVALUATION_RUN_NOT_FOUND', detail: 'missing' },
    } as never)
    // resetAllMocks also resets the shared localStorage stubs; give each test a working store.
    const stored = new Map<string, string>()
    vi.mocked(window.localStorage.getItem).mockImplementation((key) => stored.get(key) ?? null)
    vi.mocked(window.localStorage.setItem).mockImplementation((key, value) => { stored.set(key, value) })
    vi.mocked(window.localStorage.removeItem).mockImplementation((key) => { stored.delete(key) })
    signIn('student-1')
  })

  afterEach(() => {
    mountedWrappers.splice(0).forEach((wrapper) => wrapper.unmount())
    vi.unstubAllEnvs()
    vi.useRealTimers()
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

  it('sends Work back to the Work list instead of offering a second creation path', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [mockRelease] }, error: undefined as never } as never)
    const { wrapper } = await mountAt(
      { projectId: 'project-1' },
      false,
      false,
      '/researcher/environments',
    )

    await vi.waitFor(() => expect(wrapper.text()).toContain('返回 Work 列表'))
    const listLink = wrapper.get('a[href="/researcher/environments?projectId=project-1"]')
    expect(listLink.text()).toContain('返回 Work 列表')
    expect(wrapper.text()).toContain('请在 Work 列表中选择已有 Work，或选择模板新建 Work。')
    // Work is created through a resource request; the console never creates it directly.
    expect(wrapper.text()).not.toContain('已发布环境模板')
    expect(wrapper.findAll('button').some((button) => button.text() === '创建环境')).toBe(false)
    expect(wrapper.get('.environment-id-input-details').attributes('open')).toBeUndefined()
    expect(wrapper.get('.environment-id-input-details summary').text()).toContain('高级')
  })

  it('points a student without an environment to the resource request flow instead of creating one', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [mockRelease] },
      error: undefined as never,
    })
    const { wrapper } = await mountAt()
    await vi.waitFor(() => expect(wrapper.text()).toContain('新环境需要先提交资源申请，审批通过后再进入。'))
    const listLink = wrapper.get('a[href="/student/labs?projectId=project-1"]')
    expect(listLink.text()).toContain('选择已有实验环境')
    // The console lists no release to create from and never calls create.
    expect(wrapper.text()).not.toContain('release-1')
    expect(wrapper.findAll('button').some((button) => button.text().includes('创建'))).toBe(false)
    expect(createEnvironment).not.toHaveBeenCalled()
  })
  it('loads environment console when environmentId query is present', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [] },
      error: undefined as never,
    })
    vi.mocked(getEnvironment).mockResolvedValue({
      data: {
        id: 'env-1',
        connection: { instanceReady: true, state: 'ready', guestInstructions: [] },
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

  it('shows permanent material retention without inventing a deadline', async () => {
    mockEnvironmentInstance({ eligibilityExpiresAt: null })

    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.text()).toContain('材料保留至'))
    expect(wrapper.text()).toContain('不过期，直到明确撤回')
    expect(wrapper.text()).not.toContain('Invalid Date')
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
        data: { items: [{
          id: 'request-1',
          projectId: 'project-1',
          target: { kind: 'environment', environmentId: 'env-1', releaseId: 'release-1', releaseVersion: 1 },
          state: 'active',
        }], nextCursor: null },
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
    expect(listProjectResourceRequests).toHaveBeenCalledWith({ path: { projectId: 'project-1' }, query: { limit: 100 } })
    expect(listProjectResourceLeases).toHaveBeenCalledWith({ path: { projectId: 'project-1' } })
    expect(getEnvironment).toHaveBeenCalledTimes(1)
  })

  it('does not wait when the matching Work request belongs to another project', async () => {
    vi.mocked(getEnvironment).mockResolvedValue({
      response: { status: 404 },
      error: { diagnosticCode: 'LW_ENVIRONMENT_NOT_FOUND', detail: '环境不存在', retryable: true },
    } as never)
    vi.mocked(listProjectResourceRequests).mockResolvedValue({
      data: { items: [{
        id: 'request-other-project',
        projectId: 'project-2',
        target: { kind: 'environment', environmentId: 'env-1', releaseId: 'release-1', releaseVersion: 1 },
        state: 'active',
      }], nextCursor: null },
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
      data: { items: [{
        id: 'request-1',
        projectId: 'project-1',
        diagnosticCode: 'LW_ENVIRONMENT_CREATE_AGGREGATE_INVALID',
        target: { kind: 'environment', environmentId: 'env-1', releaseId: 'release-1', releaseVersion: 1 },
        state: 'rejected',
      }], nextCursor: null },
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
      data: { items: [{
        id: 'request-1',
        projectId: 'project-1',
        target: { kind: 'environment', environmentId: 'env-1', releaseId: 'release-1', releaseVersion: 1 },
        state: 'active',
      }], nextCursor: null },
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
      class: 'work',
      desiredState: 'stopped',
      observedState: 'stopped',
      operation: { ...mockOperation('succeeded', { kind: 'stop' }) },
    })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.get('#lifecycle-action-hint').text()).toContain('环境已停止'))
    const hint = wrapper.get('#lifecycle-action-hint').text()
    expect(hint).toContain('工作目录和磁盘仍保留')
    expect(hint).toContain('继续计入存储用量，直到删除')
    expect(hint).toContain('GPU 容量是否保留取决于环境类型和项目资源配置')
    // No stop-impact contract yet: the hint must not promise GPU or billing outcomes.
    expect(hint).not.toMatch(/GPU 预留(已|会)?释放|GPU 预留和 Work 资源租约会保留|已停止计量/)

    await wrapper.find('button[aria-label="删除"]').trigger('click')
    const dialog = wrapper.findComponent({ name: 'ConfirmDialog' })
    expect(dialog.props('description')).toContain('工作目录及关联容器存储或虚拟机磁盘')
    expect(dialog.props('description')).toContain('操作不可恢复')
    expect(dialog.props('description')).toContain('项目材料、已冻结提交和评测记录不在本环境删除范围内')
  })

  it('does not claim a stopped experiment released its GPU capacity', async () => {
    mockEnvironmentInstance({
      class: 'experiment',
      desiredState: 'stopped',
      observedState: 'stopped',
      operation: { ...mockOperation('succeeded', { kind: 'stop' }) },
    })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.get('#lifecycle-action-hint').text()).toContain('环境已停止'))
    const hint = wrapper.get('#lifecycle-action-hint').text()
    expect(hint).toContain('GPU 容量是否保留取决于环境类型和项目资源配置')
    expect(hint).not.toMatch(/GPU 预留(已|会)?释放|资源租约会保留/)
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

    await openTab(wrapper, 'Web 控制台')
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
    expect(wrapper.text()).toContain('收起切换')

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

  describe('unknown restart outcome (#229)', () => {
    const unknownRestart = { kind: 'restart', diagnosticCode: 'LW_ENVIRONMENT_RESTART_OUTCOME_UNKNOWN' }

    function mockUnknownRestart(retryEligible: boolean) {
      mockEnvironmentInstance({
        observedState: 'failed',
        failedPhase: 'provisioning',
        operation: { ...mockOperation('failed', unknownRestart), id: 'op-restart' },
      })
      vi.mocked(listEnvironmentOperations).mockResolvedValue({
        data: { items: [mockOperation('failed', { ...unknownRestart, retryEligible })] },
        error: undefined as never,
      } as never)
    }

    it('explains the unknown outcome before the code and sends nothing on its own', async () => {
      vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
      try {
        mockUnknownRestart(true)
        const { wrapper } = await mountAt({ environmentId: 'env-1' })
        await vi.waitFor(() => expect(wrapper.text()).toContain('上次重启结果未知，平台不会自动重试'))
        await openTab(wrapper, '异步操作与诊断')
        const panel = await vi.waitFor(() => wrapper.get('.env-failed-panel'))

        expect(panel.get('.diagnostic-message').text()).toBe('无法确认上次重启是否完成，请检查环境状态后手动选择恢复。')
        expect(panel.get('.diagnostic-next-step').text()).toContain('使用顶部操作栏的“重试失败的操作”手动恢复')
        expect(panel.get('.diagnostic-code').text()).toBe('LW_ENVIRONMENT_RESTART_OUTCOME_UNKNOWN')
        // The reason comes before the code, in the panel and in the timeline.
        expect(panel.text().indexOf('无法确认上次重启')).toBeLessThan(panel.text().indexOf('LW_ENVIRONMENT_RESTART_OUTCOME_UNKNOWN'))
        const timeline = wrapper.text()
        expect(timeline.indexOf('无法确认上次重启是否完成')).toBeLessThan(timeline.indexOf('诊断码：LW_ENVIRONMENT_RESTART_OUTCOME_UNKNOWN'))
        expect(panel.find('.diagnostic-retry').exists()).toBe(false)

        await vi.advanceTimersByTimeAsync(30_000)
        await flushPromises()
        expect(retryEnvironment).not.toHaveBeenCalled()
        expect(restartEnvironment).not.toHaveBeenCalled()
        expect(startEnvironment).not.toHaveBeenCalled()
      } finally {
        vi.useRealTimers()
      }
    })

    it('recovers only through the explicit toolbar entry, once per click', async () => {
      mockUnknownRestart(true)
      vi.mocked(retryEnvironment).mockResolvedValue({
        data: { environmentId: 'env-1', operationId: 'op-recover', revision: 12, statusUrl: '/api/v1/environments/env-1' },
        error: undefined as never,
      } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1' })
      const recover = await vi.waitFor(() => {
        const button = wrapper.findAll('button').find((candidate) => candidate.text() === '重试失败的操作')
        expect(button).toBeDefined()
        return button!
      })
      expect(retryEnvironment).not.toHaveBeenCalled()

      await recover.trigger('click')
      await vi.waitFor(() => expect(retryEnvironment).toHaveBeenCalledTimes(1))
      expect(retryEnvironment).toHaveBeenCalledWith({
        path: { environmentId: 'env-1' },
        headers: { 'If-Match': '"rev-11"', 'Idempotency-Key': expect.any(String) },
      })
      expect(restartEnvironment).not.toHaveBeenCalled()
    })

    it('says no recovery is available when the server no longer offers one', async () => {
      mockUnknownRestart(false)
      const { wrapper } = await mountAt({ environmentId: 'env-1' })
      await vi.waitFor(() => expect(wrapper.text()).toContain('上次重启结果未知'))
      await openTab(wrapper, '异步操作与诊断')
      const panel = await vi.waitFor(() => wrapper.get('.env-failed-panel'))

      expect(panel.get('.diagnostic-message').text()).toBe('无法确认上次重启是否完成，请检查环境状态后手动选择恢复。')
      expect(panel.get('.diagnostic-next-step').text()).toContain('当前没有可用的恢复操作')
      expect(wrapper.findAll('button').some((button) => button.text() === '重试失败的操作')).toBe(false)
      expect(retryEnvironment).not.toHaveBeenCalled()
    })
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
    vi.useFakeTimers({ shouldAdvanceTime: true, now: Date.parse('2026-07-11T12:00:00.000Z') })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.find('.runtime-access').exists()).toBe(true))
    expect(wrapper.find('.endpoint-grants').text()).toContain('http')
    const openButton = wrapper.findAll('button').find((button) => button.text() === '打开容器实验')
    expect(openButton).toBeDefined()
    await openButton!.trigger('click')
    expect(open).toHaveBeenCalledWith('/connect/endpoint-grant-http/', '_blank', 'noopener,noreferrer')
    open.mockRestore()
    vi.useRealTimers()
  })

  it.each([
    ['stopped', 'stopped', '环境已停止，启动后才能签发访问授权。'],
    ['failed', 'running', '环境处于失败状态，重试成功并恢复就绪后才能签发访问授权。'],
  ] as const)('does not offer terminal access grant when the environment is not ready (%s)', async (observedState, desiredState, hint) => {
    mockEnvironmentInstance({ observedState, desiredState })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await openTab(wrapper, 'Web 控制台')
    const pane = wrapper.get('.console-unauthorized-pane')
    expect(pane.text()).toContain(hint)
    expect(pane.text()).not.toContain('一键签发')
    expect(pane.find('button').exists()).toBe(false)
  })

  function mockActions(actions: string[], projectId = 'project-1') {
    vi.mocked(getProjectAvailableActions).mockResolvedValue({
      data: { projectId, actorId: 'actor-1', membershipRevision: 4, validUntil: '2099-01-01T00:00:00.000Z', actions },
      error: undefined,
    } as never)
  }

  function lifecycleButton(wrapper: { findAll: (selector: string) => Array<{ attributes: (name: string) => string | undefined }> }, label: string) {
    return wrapper.findAll('button').find((button) => button.attributes('aria-label') === label)
  }

  it('reads actions for the environment\'s own project and offers an operator only start, stop and restart', async () => {
    mockActions(['browse', 'start', 'stop'])
    mockEnvironmentInstance({ operation: { id: 'op-current', kind: 'start', state: 'succeeded' } })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(lifecycleButton(wrapper, '停止')?.attributes('disabled')).toBeUndefined())
    expect(vi.mocked(getProjectAvailableActions)).toHaveBeenLastCalledWith({ path: { projectId: 'project-1' } })
    expect(lifecycleButton(wrapper, '重启')?.attributes('disabled')).toBeUndefined()
    const remove = lifecycleButton(wrapper, '删除')
    expect(remove?.attributes('disabled')).toBeDefined()
    // A disabled button never needs a hover: a visible hint says why.
    expect(remove?.attributes('title')).toBeTruthy()
    expect(wrapper.find('[data-testid="lifecycle-permission-hint"]').exists()).toBe(true)
    // An operator holds no grant_access: the console explains instead of offering a grant.
    await wrapper.findAll('button').find((button) => button.text().includes('Web 控制台'))!.trigger('click')
    const pane = wrapper.get('.console-unauthorized-pane')
    expect(pane.find('.access-grant-hint').exists()).toBe(true)
    expect(pane.find('button').exists()).toBe(false)
  })

  it('gives a viewer no lifecycle, grant or submission action and never sends one', async () => {
    mockActions(['browse'])
    mockEnvironmentInstance({ operation: { id: 'op-current', kind: 'start', state: 'succeeded' } })
    vi.mocked(listEnvironmentEndpoints).mockResolvedValue({
      data: { items: [{ id: 'ep-ssh', protocol: 'ssh', health: 'healthy', observedAt: '2026-07-11T10:00:00.000Z' }] },
      error: undefined as never,
    } as never)
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [{ ...mockRelease, submissionManifest: mockSubmissionManifest }] },
      error: undefined as never,
    } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.find('[data-testid="lifecycle-permission-hint"]').exists()).toBe(true))
    for (const label of ['启动', '停止', '重启', '删除']) {
      expect(lifecycleButton(wrapper, label)?.attributes('disabled')).toBeDefined()
    }
    expect(lifecycleButton(wrapper, '停止')?.attributes('title')).toBeTruthy()
    await vi.waitFor(() => expect(wrapper.find('[data-testid="grant-permission-hint"]').exists()).toBe(true))
    expect(wrapper.findAll('button').some((button) => button.text().includes('签发授权并连接'))).toBe(false)
    expect(wrapper.findAll('button').some((button) => button.text() === '签发访问授权')).toBe(false)

    await wrapper.findAll('button').find((button) => button.text().includes('提交'))!.trigger('click')
    await flushPromises()
    expect(wrapper.find('.freeze-unavailable').exists()).toBe(true)
    expect(wrapper.findAll('button').find((button) => button.text() === '发起冻结提交')?.attributes('disabled')).toBeDefined()

    expect(stopEnvironment).not.toHaveBeenCalled()
    expect(deleteEnvironment).not.toHaveBeenCalled()
    expect(createAccessGrant).not.toHaveBeenCalled()
    expect(freezeSubmission).not.toHaveBeenCalled()
  })

  it('keeps every write closed while the project permissions cannot be read and shows the reason', async () => {
    vi.mocked(getProjectAvailableActions).mockResolvedValue({
      data: undefined,
      error: { type: 'about:blank', title: 'Unavailable', status: 503, detail: '成员服务暂时不可用。', diagnosticCode: 'LW_AUTH_MEMBERSHIP_UNAVAILABLE', retryable: true },
    } as never)
    mockEnvironmentInstance({ operation: { id: 'op-current', kind: 'start', state: 'succeeded' } })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })

    await vi.waitFor(() => expect(wrapper.find('[data-testid="lifecycle-permission-hint"]').exists()).toBe(true))
    // The server's reason reaches the user while every write stays closed.
    expect(wrapper.get('[data-testid="lifecycle-permission-hint"]').text()).toContain('成员服务暂时不可用。')
    for (const label of ['启动', '停止', '重启', '删除']) {
      expect(lifecycleButton(wrapper, label)?.attributes('disabled')).toBeDefined()
    }
  })

  it('re-reads project permissions after the server refuses a lifecycle request', async () => {
    mockEnvironmentInstance({ operation: { id: 'op-current', kind: 'start', state: 'succeeded' } })
    vi.mocked(stopEnvironment).mockResolvedValue({
      data: undefined,
      error: { type: 'about:blank', title: 'Forbidden', status: 403, detail: '你已不是该项目的成员。', diagnosticCode: 'LW_AUTH_SCOPE_DENIED', retryable: false },
    } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(lifecycleButton(wrapper, '停止')?.attributes('disabled')).toBeUndefined())
    mockActions(['browse'])

    await wrapper.findAll('button').find((button) => button.attributes('aria-label') === '停止')!.trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLButtonElement>('[role="alertdialog"] .filled-button')?.click()
    await flushPromises()

    await vi.waitFor(() => expect(wrapper.text()).toContain('LW_AUTH_SCOPE_DENIED'))
    // The new answer (viewer) closes the button the stale answer had opened.
    await vi.waitFor(() => expect(lifecycleButton(wrapper, '停止')?.attributes('disabled')).toBeDefined())
    expect(wrapper.find('[data-testid="lifecycle-permission-hint"]').exists()).toBe(true)
  })

  it('offers no creation entry whatever the edit responsibility', async () => {
    for (const actions of [['browse', 'start', 'stop'], ['browse', 'edit', 'start', 'stop']]) {
      mockActions(actions)
      vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [mockRelease] }, error: undefined as never } as never)
      const { wrapper } = await mountAt({ projectId: 'project-1' })
      await vi.waitFor(() => expect(wrapper.text()).toContain('新环境需要先提交资源申请'))
      expect(wrapper.find('[data-testid="create-permission-hint"]').exists()).toBe(false)
      expect(wrapper.findAll('button').some((button) => button.text().includes('创建'))).toBe(false)
      wrapper.unmount()
    }
    expect(createEnvironment).not.toHaveBeenCalled()
  })

  it('re-checks the edit responsibility when a freeze is confirmed and closes a stale confirmation after a 403', async () => {
    mockEnvironmentInstance({ operation: { id: 'op-current', kind: 'start', state: 'succeeded' } })
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [{ ...mockRelease, submissionManifest: mockSubmissionManifest }] },
      error: undefined as never,
    } as never)
    vi.mocked(freezeSubmission).mockResolvedValue({
      data: undefined,
      error: { type: 'about:blank', title: 'Forbidden', status: 403, detail: '你已没有提交评测的权限。', diagnosticCode: 'LW_AUTH_SCOPE_DENIED', retryable: false },
    } as never)
    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await wrapper.findAll('button').find((button) => button.text().includes('提交'))!.trigger('click')
    const start = () => wrapper.findAll('button').find((button) => button.text() === '发起冻结提交')
    await vi.waitFor(() => expect(start()?.attributes('disabled')).toBeUndefined())
    await start()!.trigger('click')
    await vi.waitFor(() => expect(wrapper.find('[aria-label="确认冻结清单"]').exists()).toBe(true))
    mockActions(['browse'])

    await wrapper.findAll('button').find((button) => button.text() === '确认冻结')!.trigger('click')
    await flushPromises()

    expect(freezeSubmission).toHaveBeenCalledTimes(1)
    await vi.waitFor(() => expect(wrapper.text()).toContain('LW_AUTH_SCOPE_DENIED'))
    expect(wrapper.find('[aria-label="确认冻结清单"]').exists()).toBe(false)
    // The refreshed answer closes the submission entry, so a second attempt is not sent.
    await vi.waitFor(() => expect(start()?.attributes('disabled')).toBeDefined())
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
        connection: { instanceReady: false, state: 'unavailable', guestInstructions: [] },
        projectId: 'project-1',
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

    await vi.waitFor(() => expect(wrapper.findAll('button').find((b) => b.text() === '启动' && !b.attributes('disabled'))).toBeDefined())
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

    await openTab(wrapper, '实验提交与凭据')
    await vi.waitFor(() => expect(wrapper.text()).toContain('冻结中'))
    expect(wrapper.text()).toContain('不可变快照')
    const resultsLink = wrapper.find('a[href^="/student/results"]')
    expect(resultsLink.exists()).toBe(true)
    expect(resultsLink.attributes('href')).toContain('/student/results?projectId=project-2')

    await openTab(wrapper, 'Web 控制台')
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
    await openTab(wrapper, '实验提交与凭据')

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

    await openTab(wrapper, 'Web 控制台')
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
    await openTab(wrapper, '实验提交与凭据')

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
    await openTab(wrapper, '实验提交与凭据')

    await vi.waitFor(() => expect(wrapper.text()).toContain('当前环境所用版本已撤回'))
    const startButton = wrapper.findAll('button').find((button) => button.text() === '发起冻结提交')!
    expect(startButton.attributes('disabled')).toBeDefined()
    expect(freezeSubmission).not.toHaveBeenCalled()
  })

  function frozenSubmission(id: string, extra: Record<string, unknown> = {}) {
    return {
      data: {
        id,
        projectId: 'project-1',
        actorId: 'student-1',
        environment: { environmentId: 'env-1', environmentRevision: 12, releaseId: 'rel-1', releaseVersion: 1, runtimeKind: 'container' },
        object: { artifactId: 'artifact-1', mediaType: 'application/tar', objectVersion: 'object-version-1', sizeBytes: 123, storeBinding: 'evaluation-store' },
        contentSha256: 'b'.repeat(64),
        frozenAt: '2026-07-11T10:10:00.000Z',
        ...extra,
      },
      error: undefined,
    } as never
  }

  function runProgress(state: string, phase: string, extra: Record<string, unknown> = {}) {
    return {
      data: {
        runId: 'run-1', projectId: 'project-1', revision: 2, state, page: 1, pageSize: 20, hasMore: false, history: [],
        current: [{ stepRunId: 'step-1', progress: { attempt: 1, phase, nextSteps: [], ...extra } }],
      },
      error: undefined,
    } as never
  }

  it('remembers only the identifier of a new submission and shows its progress', async () => {
    mockEnvironmentInstance()
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [{ ...mockRelease, submissionManifest: mockSubmissionManifest }] },
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
    vi.mocked(getFrozenSubmission).mockResolvedValue(frozenSubmission('00000000-0000-7000-8000-000000000001'))

    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await openTab(wrapper, '实验提交与凭据')
    const startButton = wrapper.findAll('button').find((button) => button.text() === '发起冻结提交')!
    await vi.waitFor(() => expect(startButton.attributes('disabled')).toBeUndefined())
    await startButton.trigger('click')
    await wrapper.get('[role="dialog"]').findAll('button').find((button) => button.text() === '确认冻结')!.trigger('click')

    await vi.waitFor(() => expect(wrapper.find('[data-testid="submission-tracking"]').exists()).toBe(true))
    const tracking = wrapper.get('[data-testid="submission-tracking"]')
    await vi.waitFor(() => expect(tracking.text()).toContain('平台还没有为它创建评测'))
    expect(getOwnProjectEvaluationTaskForSubmission).toHaveBeenCalledWith({
      path: { projectId: 'project-1', submissionId: '00000000-0000-7000-8000-000000000001' },
    })
    expect(tracking.get('[aria-current="step"]').attributes('data-stage')).toBe('queued')
    expect(tracking.get('a').attributes('href')).toBe('/student/results?projectId=project-1&submissionId=00000000-0000-7000-8000-000000000001')
    // Only identifiers are stored, under the signed-in actor; names, times and states come from the server.
    expect(JSON.parse(localStorage.getItem('labweaver_submissions:student-1') ?? '{}')).toEqual({
      'project-1': ['00000000-0000-7000-8000-000000000001'],
    })
  })

  it('shows the server stage of the latest submission and links to its evaluation', async () => {
    rememberSubmission('student-1', 'project-1', 'submission-1')
    mockEnvironmentInstance()
    vi.mocked(getFrozenSubmission).mockResolvedValue(frozenSubmission('submission-1'))
    vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue(runProgress('running', 'waiting_resource_approval', { resourceRequestId: 'request-1', resourceState: 'reviewing' }))

    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await openTab(wrapper, '实验提交与凭据')
    await vi.waitFor(() => expect(wrapper.get('[data-testid="submission-tracking"]').text()).toContain('评测所需资源正在等待审批'))
    const tracking = wrapper.get('[data-testid="submission-tracking"]')
    expect(tracking.get('[aria-current="step"]').attributes('data-stage')).toBe('approval')
    expect(tracking.get('[data-stage="submitted"]').text()).toContain('保存于')
    expect(tracking.get('a').attributes('href')).toBe('/student/results/run-1?projectId=project-1&submissionId=submission-1')
    expect(tracking.text()).not.toMatch(/%/)
  })

  it('shows nothing for a remembered submission until the server returns it to this student', async () => {
    rememberSubmission('student-1', 'project-1', 'submission-1')
    mockEnvironmentInstance()
    vi.mocked(getFrozenSubmission).mockResolvedValue({ data: undefined, error: { status: 503, diagnosticCode: 'LW_X', detail: 'x', retryable: true } } as never)

    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await openTab(wrapper, '实验提交与凭据')
    await vi.waitFor(() => expect(getFrozenSubmission).toHaveBeenCalled())
    await flushPromises()
    expect(wrapper.find('[data-testid="submission-tracking"]').exists()).toBe(false)
    expect(getOwnProjectEvaluationTaskForSubmission).not.toHaveBeenCalled()
    // A failed read is not a refusal: the identifier stays for the next visit.
    expect(JSON.parse(localStorage.getItem('labweaver_submissions:student-1') ?? '{}')).toEqual({ 'project-1': ['submission-1'] })
  })

  it('drops a remembered submission the server refuses or attributes to another student', async () => {
    rememberSubmission('student-1', 'project-1', 'submission-foreign')
    rememberSubmission('student-1', 'project-1', 'submission-refused')
    mockEnvironmentInstance()
    vi.mocked(getFrozenSubmission).mockImplementation(async (options) => (options.path.submissionId === 'submission-refused'
      ? { data: undefined, error: { status: 403, diagnosticCode: 'LW_FORBIDDEN', detail: 'no' } }
      : frozenSubmission('submission-foreign', { actorId: 'student-2' })) as never)

    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await openTab(wrapper, '实验提交与凭据')
    await vi.waitFor(() => expect(localStorage.getItem('labweaver_submissions:student-1')).toBeNull())
    expect(wrapper.find('[data-testid="submission-tracking"]').exists()).toBe(false)
    expect(getOwnProjectEvaluationTaskForSubmission).not.toHaveBeenCalled()
  })

  it('does not show the previous student\'s submissions after another student signs in on the same browser', async () => {
    rememberSubmission('student-1', 'project-1', 'submission-1')
    signIn(null)
    signIn('student-2')
    mockEnvironmentInstance()
    vi.mocked(getFrozenSubmission).mockResolvedValue(frozenSubmission('submission-1', { actorId: 'student-2' }))

    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await openTab(wrapper, '实验提交与凭据')
    await flushPromises()
    expect(localStorage.getItem('labweaver_submissions:student-1')).toBeNull()
    expect(getFrozenSubmission).not.toHaveBeenCalled()
    expect(wrapper.find('[data-testid="submission-tracking"]').exists()).toBe(false)
  })

  it('keeps reading the result after the run ends until the result becomes visible', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    rememberSubmission('student-1', 'project-1', 'submission-1')
    mockEnvironmentInstance()
    vi.mocked(getFrozenSubmission).mockResolvedValue(frozenSubmission('submission-1'))
    vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue(runProgress('succeeded', 'succeeded'))
    vi.mocked(getOwnProjectEvaluationResult)
      .mockResolvedValueOnce({ data: undefined, error: { status: 404, diagnosticCode: 'LW_EVALUATION_RUN_NOT_FOUND', detail: 'missing' } } as never)
      .mockResolvedValueOnce({ data: undefined, error: { status: 503, diagnosticCode: 'LW_EVALUATION_UNAVAILABLE', detail: 'down', retryable: true } } as never)
      .mockResolvedValue({
        data: {
          runId: 'run-1', projectId: 'project-1', courseId: null, releaseId: 'rel-1', frozenSubmissionId: 'submission-1', revision: 1,
          state: 'succeeded', awardedScore: 8, maxScore: 10, createdAt: '2026-07-11T10:10:00.000Z', updatedAt: '2026-07-11T10:20:00.000Z',
          completedAt: '2026-07-11T10:20:00.000Z', steps: [],
        },
        error: undefined,
      } as never)

    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await openTab(wrapper, '实验提交与凭据')
    await vi.waitFor(() => expect(wrapper.get('[data-testid="submission-tracking"]').text()).toContain('评测已结束，正在读取结果'))
    expect(getOwnProjectEvaluationResult).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(10_000)
    await flushPromises()
    expect(getOwnProjectEvaluationResult).toHaveBeenCalledTimes(2)
    expect(wrapper.get('[data-testid="submission-tracking"]').text()).toContain('评测已结束，正在读取结果')

    await vi.advanceTimersByTimeAsync(15_000)
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.get('[data-testid="submission-tracking"]').text()).toContain('8'))
    expect(wrapper.get('[data-testid="submission-tracking"]').text()).not.toContain('正在读取结果')
    const calls = vi.mocked(getOwnProjectEvaluationResult).mock.calls.length
    await vi.advanceTimersByTimeAsync(60_000)
    expect(vi.mocked(getOwnProjectEvaluationResult).mock.calls.length).toBe(calls)
  })

  it.each([
    ['a refusal', { status: 403, diagnosticCode: 'LW_EVALUATION_RESULT_FORBIDDEN', detail: '当前账号无权读取这次评测的结果。', retryable: false }],
    ['a non-retryable server failure', { status: 500, diagnosticCode: 'LW_EVALUATION_RESULT_CORRUPT', detail: '评测结果记录损坏，无法读取。', retryable: false }],
  ])('stops reading the result after %s and explains it before the code', async (_label, problem) => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    rememberSubmission('student-1', 'project-1', 'submission-1')
    mockEnvironmentInstance()
    vi.mocked(getFrozenSubmission).mockResolvedValue(frozenSubmission('submission-1'))
    vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue(runProgress('succeeded', 'succeeded'))
    vi.mocked(getOwnProjectEvaluationResult)
      .mockResolvedValueOnce({ data: undefined, error: { status: 404, diagnosticCode: 'LW_EVALUATION_RUN_NOT_FOUND', detail: 'missing' } } as never)
      .mockResolvedValue({ data: undefined, error: problem } as never)

    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    await openTab(wrapper, '实验提交与凭据')
    await vi.waitFor(() => expect(wrapper.get('[data-testid="submission-tracking"]').text()).toContain('评测已结束，正在读取结果'))

    await vi.advanceTimersByTimeAsync(10_000)
    await flushPromises()
    const tracking = wrapper.get('[data-testid="submission-tracking"]')
    await vi.waitFor(() => expect(tracking.find('[data-testid="submission-result-failure"]').exists()).toBe(true))
    expect(tracking.text()).toContain('无法读取结果，页面已停止自动更新')
    expect(tracking.text()).not.toContain('正在读取结果')
    const banner = tracking.get('[data-testid="submission-result-failure"]').text()
    expect(banner).toContain(problem.diagnosticCode)
    expect(banner.indexOf(problem.detail)).toBeGreaterThanOrEqual(0)
    expect(banner.indexOf(problem.detail)).toBeLessThan(banner.indexOf(problem.diagnosticCode))

    const calls = vi.mocked(getOwnProjectEvaluationResult).mock.calls.length
    await vi.advanceTimersByTimeAsync(120_000)
    expect(vi.mocked(getOwnProjectEvaluationResult).mock.calls.length).toBe(calls)
  })

  it('exposes the detail sections as an accessible tab list', async () => {
    mockEnvironmentInstance()
    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    const tablist = wrapper.get('[role="tablist"]')
    expect(tablist.attributes('aria-label')).toBe('环境资源详情')
    const tabs = tablist.findAll('[role="tab"]')
    expect(tabs.map((tab) => tab.text())).toEqual(['概览与访问', 'Web 控制台', '异步操作与诊断', '实验提交与凭据'].map((label) => expect.stringContaining(label)))
    expect(tabs[0].attributes('aria-selected')).toBe('true')
    const panel = wrapper.get(`#${tabs[0].attributes('aria-controls')}`)
    expect(panel.attributes('role')).toBe('tabpanel')
    await openTab(wrapper, '实验提交与凭据')
    expect(tabs[3].attributes('aria-selected')).toBe('true')
    // Inactive panels stay mounted so an open console session is not torn down.
    expect(panel.attributes('hidden')).toBeDefined()
    expect(wrapper.findAll('[role="tabpanel"]').length).toBe(4)
  })

  it('activates a detail tab from the keyboard', async () => {
    mockEnvironmentInstance()
    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    const tabs = () => wrapper.findAll('[role="tab"]')
    await tabs()[2].trigger('keydown', { key: 'Enter' })
    expect(tabs()[2].attributes('aria-selected')).toBe('true')
    expect(wrapper.get(`#${tabs()[2].attributes('aria-controls')}`).attributes('hidden')).toBeUndefined()
    await tabs()[1].trigger('keydown', { key: ' ' })
    expect(tabs()[1].attributes('aria-selected')).toBe('true')
    expect(tabs()[2].attributes('aria-selected')).toBe('false')
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
    await openTab(wrapper, '实验提交与凭据')
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

  describe('lifecycle confirmation', () => {
    function lifecycleDialog(wrapper: Awaited<ReturnType<typeof mountAt>>['wrapper']) {
      return wrapper.findAllComponents({ name: 'ConfirmDialog' }).find((dialog) => ['停止环境', '重启环境'].includes(dialog.props('title')))!
    }

    beforeEach(() => {
      mockEnvironmentInstance({ operation: { ...mockOperation('succeeded', { kind: 'start' }) } })
      vi.mocked(stopEnvironment).mockResolvedValue({ data: { environmentId: 'env-1', operationId: 'op-stop', revision: 12 }, error: undefined } as never)
      vi.mocked(restartEnvironment).mockResolvedValue({ data: { environmentId: 'env-1', operationId: 'op-restart', revision: 12 }, error: undefined } as never)
    })

    it.each([
      ['停止', stopEnvironment, '环境实际停止后，计算用量才停止计量'],
      ['重启', restartEnvironment, '重启完成前无法连接'],
    ] as const)('asks before %s and sends nothing until confirmed', async (label, sdkCall, impact) => {
      const { wrapper } = await mountAt({ environmentId: 'env-1' })
      await vi.waitFor(() => expect((wrapper.find(`button[aria-label="${label}"]`).element as HTMLButtonElement).disabled).toBe(false))

      await wrapper.find(`button[aria-label="${label}"]`).trigger('click')
      const dialog = lifecycleDialog(wrapper)
      expect(dialog.props('open')).toBe(true)
      expect(dialog.props('description')).toContain('会断开当前终端、桌面和 Web 访问会话')
      expect(dialog.props('description')).toContain(impact)
      expect(sdkCall).not.toHaveBeenCalled()

      dialog.vm.$emit('confirm')
      await vi.waitFor(() => expect(sdkCall).toHaveBeenCalledTimes(1))
      expect(lifecycleDialog(wrapper).props('open')).toBe(false)
    })

    it('cancelling the stop confirmation keeps the environment running', async () => {
      const { wrapper } = await mountAt({ environmentId: 'env-1' })
      await vi.waitFor(() => expect((wrapper.find('button[aria-label="停止"]').element as HTMLButtonElement).disabled).toBe(false))
      await wrapper.find('button[aria-label="停止"]').trigger('click')
      lifecycleDialog(wrapper).vm.$emit('cancel')
      await flushPromises()
      expect(lifecycleDialog(wrapper).props('open')).toBe(false)
      expect(stopEnvironment).not.toHaveBeenCalled()
    })

    it('keeps stop impact conditional instead of promising GPU or billing outcomes', async () => {
      const { wrapper } = await mountAt({ environmentId: 'env-1' })
      await vi.waitFor(() => expect((wrapper.find('button[aria-label="停止"]').element as HTMLButtonElement).disabled).toBe(false))
      await wrapper.find('button[aria-label="停止"]').trigger('click')
      const description = lifecycleDialog(wrapper).props('description') as string
      expect(description).toContain('提交停止不代表已停止计量')
      expect(description).toContain('GPU 容量是否保留取决于环境类型和项目资源配置')
      expect(description).not.toMatch(/GPU 预留(会|已)?释放|资源租约会保留|不再计量|存储费率/)
    })
  })

  describe('connection readiness', () => {
    const NOW = Date.parse('2026-07-11T12:00:00.000Z')
    const healthyEndpoint = { id: 'endpoint-1', protocol: 'https', health: 'healthy', revision: 1, observedAt: '2026-07-11T10:00:00.000Z' }
    const sshEndpoint = { ...healthyEndpoint, id: 'endpoint-ssh', protocol: 'ssh' }

    function readiness(wrapper: Awaited<ReturnType<typeof mountAt>>['wrapper']) {
      return wrapper.get('[data-testid="connection-summary"]').attributes('data-readiness')
    }

    function methodIds(wrapper: Awaited<ReturnType<typeof mountAt>>['wrapper']) {
      return wrapper.get('[data-testid="connection-summary"]').findAll('[data-method]').map((item) => item.attributes('data-method'))
    }

    function mockActiveGrant(expiresAt: string) {
      vi.mocked(listEnvironmentAccessGrants).mockResolvedValue({ data: { items: [{ id: 'grant-1' }] }, error: undefined as never } as never)
      vi.mocked(getAccessGrant).mockResolvedValue({ data: activeGrant(expiresAt), error: undefined as never } as never)
    }

    beforeEach(() => {
      vi.useFakeTimers({ shouldAdvanceTime: true, now: NOW })
    })

    it('separates a running instance from a connection that still needs an access grant', async () => {
      mockEnvironmentInstance({ class: 'work', courseId: null })
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [healthyEndpoint] }, error: undefined as never } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')

      await vi.waitFor(() => expect(readiness(wrapper)).toBe('needs-grant'))
      const summary = wrapper.get('[data-testid="connection-summary"]')
      expect(summary.get('[data-testid="instance-state"]').text()).toBe('运行中')
      expect(summary.get('[data-testid="connection-state"]').text()).toBe('待授权')
      expect(summary.text()).toContain('签发访问授权后即可连接')
      expect(summary.findAll('button').some((button) => button.text() === '签发授权并连接')).toBe(true)
      // Browser consoles are listed only from the grant's console capability.
      expect(methodIds(wrapper)).toEqual(['app'])
      expect(listConsoleCapabilities).not.toHaveBeenCalled()
      expect(wrapper.get('.env-meta-grid').text()).toContain('固定使用 v1')
    })

    it('does not call a stopped instance connectable', async () => {
      mockEnvironmentInstance({ desiredState: 'stopped', observedState: 'stopped' })
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' })

      await vi.waitFor(() => expect(readiness(wrapper)).toBe('instance-not-ready'))
      const summary = wrapper.get('[data-testid="connection-summary"]')
      expect(summary.get('[data-testid="instance-state"]').text()).toBe('已停止')
      expect(summary.text()).toContain('环境已停止，启动后才能签发访问授权。')
      expect(summary.findAll('button').some((button) => button.text() === '打开 Web 终端')).toBe(false)
    })

    it('re-reads the server connection facts while SSH is unconfirmed and never infers readiness from endpoints', async () => {
      mockEnvironmentInstance({
        runtimeKind: 'virtual_machine',
        connection: { instanceReady: true, state: 'unknown', reasonCode: 'LW_ENVIRONMENT_SSH_HANDSHAKE_PENDING', observedAt: '2026-07-11T11:59:00.000Z', guestInstructions: [] },
      })
      // A healthy endpoint row alone does not make the connection ready.
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [sshEndpoint] }, error: undefined as never } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' })

      await vi.waitFor(() => expect(readiness(wrapper)).toBe('unknown'))
      const summary = wrapper.get('[data-testid="connection-summary"]')
      expect(summary.get('[data-testid="instance-state"]').text()).toBe('运行中')
      expect(summary.get('[data-testid="connection-state"]').text()).toBe('无法确认')
      expect(summary.text()).toContain('SSH 还没有完成握手')
      expect(summary.text()).toContain('页面每隔几秒自动重新检查')
      expect(summary.get('[data-testid="connection-reason"]').text()).toContain('LW_ENVIRONMENT_SSH_HANDSHAKE_PENDING')
      expect(summary.findAll('button').some((button) => button.text() === '签发授权并连接')).toBe(false)
      const environmentReads = vi.mocked(getEnvironment).mock.calls.length

      mockEnvironmentInstance({
        runtimeKind: 'virtual_machine',
        connection: { instanceReady: true, state: 'ready', guestInstructions: ['使用镜像说明中的 guest 账号登录桌面。', '通过平台网关 SSH 连接，先登记个人公钥。'] },
      })
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [sshEndpoint] }, error: undefined as never } as never)
      await vi.advanceTimersByTimeAsync(5000)
      await flushPromises()

      expect(vi.mocked(getEnvironment).mock.calls.length).toBeGreaterThan(environmentReads)
      await vi.waitFor(() => expect(readiness(wrapper)).toBe('needs-grant'))
      const guest = wrapper.get('[data-testid="guest-instructions"]')
      expect(guest.text()).toContain('平台账号只用于签发访问授权，不会自动登录环境内的操作系统')
      expect(guest.findAll('li').map((item) => item.text())).toEqual(['使用镜像说明中的 guest 账号登录桌面。', '通过平台网关 SSH 连接，先登记个人公钥。'])
      expect(wrapper.find('[data-testid="connection-reason"]').exists()).toBe(false)
    })

    it('reports a connection still being prepared separately from instance readiness', async () => {
      mockEnvironmentInstance({ connection: { instanceReady: true, state: 'preparing', guestInstructions: [] } })
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [{ ...healthyEndpoint, health: 'pending' }] }, error: undefined as never } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' })

      await vi.waitFor(() => expect(readiness(wrapper)).toBe('connection-preparing'))
      expect(wrapper.get('[data-testid="connection-state"]').text()).toBe('连接准备中')
      expect(wrapper.get('[data-testid="instance-state"]').text()).toBe('运行中')
    })

    it('stops calling an expired grant connectable, closes the console and asks for a new grant', async () => {
      mockEnvironmentInstance({ class: 'work', courseId: null })
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [healthyEndpoint] }, error: undefined as never } as never)
      mockActiveGrant('2026-07-11T12:00:30.000Z')
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')

      await vi.waitFor(() => expect(readiness(wrapper)).toBe('ready'))
      await vi.waitFor(() => expect(methodIds(wrapper)).toEqual(['console', 'app']))
      expect(wrapper.find('.console-wrapper').exists()).toBe(true)
      expect(wrapper.find('.runtime-access').exists()).toBe(true)

      // The server keeps answering with the same grant; its expiry alone ends it.
      await vi.advanceTimersByTimeAsync(32_000)
      await flushPromises()

      await vi.waitFor(() => expect(readiness(wrapper)).toBe('needs-regrant'))
      expect(wrapper.get('[data-testid="connection-state"]').text()).toBe('待重新授权')
      expect(wrapper.find('.console-wrapper').exists()).toBe(false)
      expect(wrapper.find('.runtime-access').exists()).toBe(false)
      expect(methodIds(wrapper)).toEqual(['app'])
      expect(wrapper.get('[data-testid="connection-summary"]').findAll('button').some((button) => button.text() === '重新签发并连接')).toBe(true)
      // The expiry triggered a fresh server read of the current grant.
      expect(vi.mocked(listEnvironmentAccessGrants).mock.calls.length).toBeGreaterThan(1)
    })

    it('re-reads when the page becomes visible again and pauses while hidden', async () => {
      mockEnvironmentInstance({ connection: { instanceReady: true, state: 'unavailable', guestInstructions: [] } })
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [{ ...healthyEndpoint, health: 'unhealthy' }] }, error: undefined as never } as never)
      const visibility = vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible')
      try {
        const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' })
        await vi.waitFor(() => expect(readiness(wrapper)).toBe('connection-unavailable'))

        visibility.mockReturnValue('hidden')
        document.dispatchEvent(new Event('visibilitychange'))
        const hiddenReads = vi.mocked(listEnvironmentEndpoints).mock.calls.length
        await vi.advanceTimersByTimeAsync(20_000)
        expect(vi.mocked(listEnvironmentEndpoints).mock.calls.length).toBe(hiddenReads)

        mockEnvironmentInstance()
        vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [healthyEndpoint] }, error: undefined as never } as never)
        visibility.mockReturnValue('visible')
        document.dispatchEvent(new Event('visibilitychange'))
        await flushPromises()

        await vi.waitFor(() => expect(readiness(wrapper)).toBe('needs-grant'))
        expect(vi.mocked(listEnvironmentEndpoints).mock.calls.length).toBeGreaterThan(hiddenReads)
      } finally {
        visibility.mockRestore()
      }
    })

    it('does not run the expiry re-read while hidden, and re-arms it when the page is visible again', async () => {
      mockEnvironmentInstance({ class: 'work', courseId: null })
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [healthyEndpoint] }, error: undefined as never } as never)
      mockActiveGrant('2026-07-11T12:00:30.000Z')
      const visibility = vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible')
      try {
        const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
        await vi.waitFor(() => expect(readiness(wrapper)).toBe('ready'))

        visibility.mockReturnValue('hidden')
        document.dispatchEvent(new Event('visibilitychange'))
        const hiddenReads = vi.mocked(listEnvironmentAccessGrants).mock.calls.length
        await vi.advanceTimersByTimeAsync(40_000)
        expect(vi.mocked(listEnvironmentAccessGrants).mock.calls.length).toBe(hiddenReads)

        visibility.mockReturnValue('visible')
        document.dispatchEvent(new Event('visibilitychange'))
        await flushPromises()
        await vi.waitFor(() => expect(readiness(wrapper)).toBe('needs-regrant'))
        expect(vi.mocked(listEnvironmentAccessGrants).mock.calls.length).toBeGreaterThan(hiddenReads)
      } finally {
        visibility.mockRestore()
      }
    })

    it('issues the grant and then opens the first browser console the server permits', async () => {
      mockEnvironmentInstance({ class: 'work', courseId: null, runtimeKind: 'virtual_machine' })
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [sshEndpoint] }, error: undefined as never } as never)
      vi.mocked(createAccessGrant).mockResolvedValue({ data: activeGrant('2026-07-12T10:00:00.000Z'), error: undefined as never } as never)
      vi.mocked(listConsoleCapabilities).mockResolvedValue({ data: consoleAvailability(['novnc']), error: undefined as never } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')

      await vi.waitFor(() => expect(readiness(wrapper)).toBe('needs-grant'))
      await wrapper.get('[data-testid="connection-summary"]').findAll('button').find((button) => button.text() === '签发授权并连接')!.trigger('click')

      await vi.waitFor(() => expect(readiness(wrapper)).toBe('ready'))
      await vi.waitFor(() => expect(wrapper.get('[role="tab"][aria-selected="true"]').text()).toContain('Web 控制台'))
      const panel = wrapper.findComponent({ name: 'ConsolePanel' })
      expect(panel.exists()).toBe(true)
      expect(panel.props('kind')).toBe('novnc')
      expect(createAccessGrant).toHaveBeenCalledTimes(1)
    })

    it('stays on the overview after issuing when the server permits no browser console', async () => {
      mockEnvironmentInstance({ class: 'work', courseId: null, runtimeKind: 'virtual_machine' })
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [sshEndpoint] }, error: undefined as never } as never)
      vi.mocked(createAccessGrant).mockResolvedValue({ data: activeGrant('2026-07-12T10:00:00.000Z'), error: undefined as never } as never)
      vi.mocked(listConsoleCapabilities).mockResolvedValue({ data: consoleAvailability([]), error: undefined as never } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')

      await vi.waitFor(() => expect(readiness(wrapper)).toBe('needs-grant'))
      await wrapper.get('[data-testid="connection-summary"]').findAll('button').find((button) => button.text() === '签发授权并连接')!.trigger('click')

      await vi.waitFor(() => expect(wrapper.get('[data-testid="console-capability-hint"]').text()).toContain('没有浏览器终端或桌面'))
      expect(wrapper.get('[role="tab"][aria-selected="true"]').text()).toContain('概览与访问')
      expect(methodIds(wrapper)).toEqual(['ssh'])
    })

    it('lists only SSH for a VM whose grant permits no desktop', async () => {
      mockEnvironmentInstance({ runtimeKind: 'virtual_machine' })
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [sshEndpoint] }, error: undefined as never } as never)
      mockActiveGrant('2026-07-12T10:00:00.000Z')
      vi.mocked(listConsoleCapabilities).mockResolvedValue({ data: consoleAvailability([]), error: undefined as never } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' })

      await vi.waitFor(() => expect(readiness(wrapper)).toBe('ready'))
      await vi.waitFor(() => expect(wrapper.get('[data-testid="console-capability-hint"]').text()).toContain('没有浏览器终端或桌面'))
      expect(methodIds(wrapper)).toEqual(['ssh'])
      expect(wrapper.text()).not.toContain('打开桌面')
    })

    it('offers the desktop for a VM whose grant permits noVNC', async () => {
      mockEnvironmentInstance({ runtimeKind: 'virtual_machine' })
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [sshEndpoint] }, error: undefined as never } as never)
      mockActiveGrant('2026-07-12T10:00:00.000Z')
      vi.mocked(listConsoleCapabilities).mockResolvedValue({ data: consoleAvailability(['novnc']), error: undefined as never } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' })

      await vi.waitFor(() => expect(methodIds(wrapper)).toEqual(['desktop', 'ssh']))
      const openDesktop = wrapper.get('[data-testid="connection-summary"]').findAll('button').find((button) => button.text() === '打开桌面')!
      expect((openDesktop.element as HTMLButtonElement).disabled).toBe(false)
      expect(listConsoleCapabilities).toHaveBeenCalledWith({ path: { grantId: 'grant-1' } })
    })

    it('omits the web terminal for a container whose grant permits no xterm', async () => {
      mockEnvironmentInstance()
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [healthyEndpoint] }, error: undefined as never } as never)
      mockActiveGrant('2026-07-12T10:00:00.000Z')
      vi.mocked(listConsoleCapabilities).mockResolvedValue({ data: consoleAvailability([]), error: undefined as never } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' })

      await vi.waitFor(() => expect(wrapper.get('[data-testid="console-capability-hint"]').text()).toContain('没有浏览器终端或桌面'))
      expect(methodIds(wrapper)).toEqual(['app'])
      expect(wrapper.text()).not.toContain('打开 Web 终端')
    })
  })

  describe('Work template upgrade and lease', () => {
    const settledOperation = {
      id: 'op-current', acceptedAt: '2026-07-11T10:00:00.000Z', acceptedRevision: 11, actorId: 'student-1', attempt: 1,
      deadlineAt: '2026-07-11T10:05:00.000Z', kind: 'start', maxAttempts: 3, nextAttemptAt: '2026-07-11T10:00:00.000Z',
      preserveMutableDisk: false, providerStep: 0, state: 'succeeded', traceId: 'trace-current',
    }

    function workRelease(version: number, overrides: Record<string, unknown> = {}) {
      return {
        id: `release-work-${version}`, version, projectId: 'project-1', courseId: null, runtimeKind: 'container', purpose: 'work',
        name: 'Python 分析', description: `第 ${version} 版`, templateFamilyId: 'python-analysis', capabilities: ['terminal', 'files'],
        publishedAt: `2026-07-0${version}T10:00:00.000Z`, publishedBy: 'teacher-1',
        artifact: { kind: 'catalog_container', id: `artifact-${version}`, catalog_id: 'image-1', repin_generation: 1, trust_revision: 1, repository: 'registry.example/labs/python', digest: 'sha256:0123456789abcdef' },
        ...overrides,
      }
    }

    function mockWork(overrides: Record<string, unknown> = {}) {
      mockEnvironmentInstance({ class: 'work', courseId: null, releaseId: 'release-work-1', releaseVersion: 1, operation: settledOperation, ...overrides })
      vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
        data: { items: [workRelease(1), workRelease(2), workRelease(3, { templateFamilyId: 'other-family' }), workRelease(4, { withdrawal: { reasonCode: 'TEACHER_WITHDRAWN' } })], nextCursor: null },
        error: undefined as never,
      } as never)
    }

    function upgradeDialog(wrapper: VueWrapper) {
      return wrapper.findAllComponents({ name: 'ConfirmDialog' }).find((dialog) => dialog.props('title') === '升级模板并重启 Work')!
    }

    async function confirmUpgradeTo(wrapper: VueWrapper, version: number) {
      const panel = await vi.waitFor(() => wrapper.get('[data-testid="template-upgrade"]'))
      await vi.waitFor(() => expect(panel.find(`[data-testid="upgrade-to-${version}"]`).exists()).toBe(true))
      await panel.get(`[data-testid="upgrade-to-${version}"]`).trigger('click')
      upgradeDialog(wrapper).vm.$emit('confirm')
      await flushPromises()
      return panel
    }

    const upgradeRecord = (source: number, target: number) => ({
      sourceReleaseId: `release-work-${source}`, sourceReleaseVersion: source, targetReleaseId: `release-work-${target}`, targetReleaseVersion: target, dataPolicy: 'preserve_workspace',
    })

    beforeEach(() => {
      window.sessionStorage.clear()
    })

    it('offers only newer versions of the same family and explains restart and /workspace before upgrading', async () => {
      mockWork()
      vi.mocked(upgradeEnvironmentTemplate).mockResolvedValue({ data: { environmentId: 'env-1', operationId: 'op-upgrade', revision: 12, statusUrl: '/api/v1/environments/env-1/operations/op-upgrade' }, error: undefined as never } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')

      const panel = await vi.waitFor(() => wrapper.get('[data-testid="template-upgrade"]'))
      await vi.waitFor(() => expect(panel.findAll('[data-release-version]').map((item) => item.attributes('data-release-version'))).toEqual(['2']))
      expect(panel.text()).toContain('固定使用 Python 分析 v1')
      expect(panel.find('[data-testid="upgrade-unavailable"]').exists()).toBe(false)

      await panel.get('[data-testid="upgrade-to-2"]').trigger('click')
      const description = upgradeDialog(wrapper).props('description') as string
      expect(description).toContain('会重启')
      expect(description).toContain('/workspace 中的文件会保留')
      expect(description).toContain('/workspace 以外的改动')
      expect(description).toContain('会丢失')
      expect(upgradeEnvironmentTemplate).not.toHaveBeenCalled()

      const environmentReads = vi.mocked(getEnvironment).mock.calls.length
      upgradeDialog(wrapper).vm.$emit('confirm')
      await flushPromises()
      expect(upgradeEnvironmentTemplate).toHaveBeenCalledTimes(1)
      expect(upgradeEnvironmentTemplate).toHaveBeenCalledWith({
        path: { environmentId: 'env-1' },
        headers: { 'Idempotency-Key': expect.any(String), 'If-Match': '"rev-11"' },
        body: { targetReleaseId: 'release-work-2', targetReleaseVersion: 2, confirmed: true },
      })
      // Accepted: progress is read back from the instance, not assumed by the page.
      expect(vi.mocked(getEnvironment).mock.calls.length).toBeGreaterThan(environmentReads)
      expect(Object.keys(window.sessionStorage).filter((key) => key.startsWith('labweaver.pending-command:'))).toEqual([])
    })

    it('submits one upgrade for repeated clicks and reuses the request identity on retry and reload', async () => {
      mockWork()
      let finish: (value: unknown) => void = () => undefined
      vi.mocked(upgradeEnvironmentTemplate).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve }) as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const panel = await confirmUpgradeTo(wrapper, 2)
      // While the first request is in flight the action is disabled and a second confirm sends nothing.
      expect((panel.get('[data-testid="upgrade-to-2"]').element as HTMLButtonElement).disabled).toBe(true)
      upgradeDialog(wrapper).vm.$emit('confirm')
      await flushPromises()
      expect(upgradeEnvironmentTemplate).toHaveBeenCalledTimes(1)

      finish({ data: undefined, error: { status: 503, diagnosticCode: 'LW_ENVIRONMENT_LEASE_VERIFICATION_RESPONSE_INVALID', detail: 'invalid', requestId: 'req-503', retryable: true } })
      await flushPromises()
      const banner = wrapper.get('[data-testid="template-upgrade"] .diagnostic-banner')
      expect(banner.text()).toContain('平台暂时无法核验这个 Work 的资源使用授权，本次操作没有执行。')
      expect(banner.text()).toContain('req-503')
      expect(panel.get('[data-testid="upgrade-unconfirmed"]').text()).toContain('升级结果待确认')
      expect(wrapper.find('[data-testid="lease-new-request-link"]').exists()).toBe(false)
      const firstKey = vi.mocked(upgradeEnvironmentTemplate).mock.calls[0][0].headers['Idempotency-Key']

      vi.mocked(upgradeEnvironmentTemplate).mockResolvedValue({ data: undefined, error: { status: 503, diagnosticCode: 'LW_ENVIRONMENT_LEASE_VERIFICATION_RESPONSE_INVALID', detail: 'invalid', retryable: true } } as never)
      await banner.findAll('button').find((button) => button.text() === '重试')!.trigger('click')
      await flushPromises()
      expect(vi.mocked(upgradeEnvironmentTemplate).mock.calls[1][0].headers['Idempotency-Key']).toBe(firstKey)

      // A reload of the page before the server answered replays the same command.
      wrapper.unmount()
      const reloaded = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      await confirmUpgradeTo(reloaded.wrapper, 2)
      expect(vi.mocked(upgradeEnvironmentTemplate).mock.calls[2][0].headers['Idempotency-Key']).toBe(firstKey)
    })

    it('says when the family has no newer version and offers no upgrade', async () => {
      mockWork()
      vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [workRelease(1)], nextCursor: null }, error: undefined as never } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const panel = await vi.waitFor(() => wrapper.get('[data-testid="template-upgrade"]'))
      await vi.waitFor(() => expect(panel.text()).toContain('这个模板系列暂时没有更新的版本。'))
      expect(panel.findAll('button')).toHaveLength(0)
      expect(upgradeEnvironmentTemplate).not.toHaveBeenCalled()
    })

    it('reports an inactive lease as needing a new resource request, with an entry to it', async () => {
      mockWork()
      vi.mocked(upgradeEnvironmentTemplate).mockResolvedValue({
        data: undefined,
        error: { status: 409, diagnosticCode: 'LW_RUNTIME_LEASE_NOT_ACTIVE', detail: 'lease not active', requestId: 'req-409', retryable: false },
      } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const panel = await confirmUpgradeTo(wrapper, 2)

      const text = panel.text()
      expect(text).toContain('资源使用授权已到期、被回收或尚未生效')
      expect(text).toContain('请提交新的资源申请')
      expect(text).not.toContain('数据已被其他操作更新')
      const link = panel.get('[data-testid="lease-new-request-link"]')
      expect(link.attributes('href')).toContain('/researcher/resources')
      expect(link.attributes('href')).toContain('releaseId=release-work-1')
    })

    it('explains a resource or permission change and links a new resource request for the target version', async () => {
      mockWork()
      vi.mocked(upgradeEnvironmentTemplate).mockResolvedValue({
        data: undefined,
        error: { status: 409, diagnosticCode: 'LW_WORK_TEMPLATE_UPGRADE_REQUIRES_RESOURCE_APPROVAL', detail: 'requires approval', requestId: 'req-approval', retryable: false },
      } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const panel = await confirmUpgradeTo(wrapper, 2)

      const banner = panel.get('.diagnostic-banner')
      expect(banner.text()).toContain('资源或权限变化需要新的资源申请，不能在原 Work 内升级')
      expect(banner.text()).toContain('请为目标版本提交新的资源申请')
      expect(banner.text().indexOf('资源或权限变化')).toBeLessThan(banner.text().indexOf('LW_WORK_TEMPLATE_UPGRADE_REQUIRES_RESOURCE_APPROVAL'))
      expect(banner.text()).toContain('req-approval')
      expect(banner.findAll('button').some((button) => button.text() === '重试')).toBe(false)
      expect(panel.find('[data-testid="upgrade-unconfirmed"]').exists()).toBe(false)
      const link = panel.get('[data-testid="upgrade-new-request-link"]')
      expect(link.text()).toBe('为 Python 分析 v2 提交新的资源申请')
      expect(link.attributes('href')).toBe('/researcher/resources?projectId=project-1&releaseId=release-work-2&releaseVersion=2')
      expect(panel.find('[data-testid="lease-new-request-link"]').exists()).toBe(false)
      expect(panel.find('[data-testid="upgrade-refresh-releases"]').exists()).toBe(false)
    })

    it.each([
      ['LW_WORK_TEMPLATE_UPGRADE_FAMILY_MISMATCH', '所选版本不属于这个 Work 的模板系列'],
      ['LW_WORK_TEMPLATE_UPGRADE_RELEASE_UNAVAILABLE', '所选版本未发布、已撤回或当前不可用'],
    ])('asks to pick another same-family version after %s and refreshes the version list', async (diagnosticCode, reason) => {
      mockWork()
      vi.mocked(upgradeEnvironmentTemplate).mockResolvedValue({
        data: undefined,
        error: { status: 409, diagnosticCode, detail: 'refused', requestId: 'req-target', retryable: true },
      } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const panel = await confirmUpgradeTo(wrapper, 2)

      const banner = panel.get('.diagnostic-banner')
      expect(banner.text()).toContain(reason)
      expect(banner.text()).toContain('请刷新版本列表，重新选择同一模板系列中可用的新版本。')
      expect(banner.text().indexOf(reason)).toBeLessThan(banner.text().indexOf(diagnosticCode))
      expect(banner.findAll('button').some((button) => button.text() === '重试')).toBe(false)
      expect(panel.find('[data-testid="upgrade-unconfirmed"]').exists()).toBe(false)
      expect(panel.find('[data-testid="upgrade-new-request-link"]').exists()).toBe(false)

      const releaseReads = vi.mocked(listEnvironmentTemplateReleases).mock.calls.length
      // The refreshed list no longer has v2 but has a newer v5 of the same family.
      vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
        data: { items: [workRelease(1), workRelease(5), workRelease(2, { withdrawal: { reasonCode: 'TEACHER_WITHDRAWN' } })], nextCursor: null },
        error: undefined as never,
      } as never)
      await panel.get('[data-testid="upgrade-refresh-releases"]').trigger('click')
      await flushPromises()
      expect(vi.mocked(listEnvironmentTemplateReleases).mock.calls.length).toBeGreaterThan(releaseReads)
      expect(panel.find('.diagnostic-banner').exists()).toBe(false)
      expect(panel.findAll('[data-release-version]').map((item) => item.attributes('data-release-version'))).toEqual(['5'])
      expect(upgradeEnvironmentTemplate).toHaveBeenCalledTimes(1)
    })

    it('asks for a new resource request when the old approval has no permission projection and offers no retry', async () => {
      mockWork()
      vi.mocked(upgradeEnvironmentTemplate).mockResolvedValue({
        data: undefined,
        // Even a transport that marks it retryable cannot make an old approval authorize new compute.
        error: { status: 409, diagnosticCode: 'LW_RUNTIME_AUTHORIZATION_REAPPROVAL_REQUIRED', detail: '旧资源批准没有权限投影，请重新提交资源申请', requestId: 'req-reapproval', retryable: true },
      } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const panel = await confirmUpgradeTo(wrapper, 2)

      const banner = panel.get('.diagnostic-banner')
      expect(banner.text()).toContain('旧资源批准没有权限投影')
      expect(banner.text()).toContain('请重新提交资源申请')
      expect(banner.text()).toContain('重试原操作不会成功')
      expect(banner.text().indexOf('旧资源批准')).toBeLessThan(banner.text().indexOf('LW_RUNTIME_AUTHORIZATION_REAPPROVAL_REQUIRED'))
      expect(banner.findAll('button').some((button) => button.text() === '重试')).toBe(false)
      expect(panel.find('[data-testid="upgrade-unconfirmed"]').exists()).toBe(false)
      expect(panel.get('[data-testid="upgrade-new-request-link"]').attributes('href')).toContain('/researcher/resources?projectId=project-1')
      expect(upgradeEnvironmentTemplate).toHaveBeenCalledTimes(1)
    })

    it('asks for a new resource request when a start is refused for an approval without permission projection', async () => {
      mockWork({ desiredState: 'stopped', observedState: 'stopped', operation: { ...settledOperation, kind: 'stop' } })
      vi.mocked(startEnvironment).mockResolvedValue({
        data: undefined,
        error: { status: 409, diagnosticCode: 'LW_RUNTIME_AUTHORIZATION_REAPPROVAL_REQUIRED', detail: 'reapproval', requestId: 'req-start', retryable: true },
      } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      await vi.waitFor(() => expect((wrapper.find('button[aria-label="启动"]').element as HTMLButtonElement).disabled).toBe(false))
      await wrapper.get('button[aria-label="启动"]').trigger('click')
      await flushPromises()

      const result = wrapper.get('.lifecycle-result')
      expect(result.text()).toContain('旧资源批准没有权限投影')
      expect(result.findAll('button').some((button) => button.text() === '重试')).toBe(false)
      expect(result.get('[data-testid="lease-new-request-link"]').attributes('href')).toContain('/researcher/resources')
    })

    it('shows a running upgrade from the instance operation and never calls the target version complete before success', async () => {
      mockWork({
        releaseId: 'release-work-2',
        releaseVersion: 2,
        observedState: 'updating',
        operation: { ...settledOperation, kind: 'restart', state: 'running', templateUpgrade: upgradeRecord(1, 2) },
      })
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const status = await vi.waitFor(() => wrapper.get('[data-testid="upgrade-status"]'))
      expect(status.attributes('data-state')).toBe('running')
      expect(status.text()).toContain('模板升级 v1 → 目标版本 v2')
      expect(status.text()).toContain('/workspace 中的文件保留')
      expect(status.text()).not.toContain('升级已完成')
      const panel = wrapper.get('[data-testid="template-upgrade"]')
      await vi.waitFor(() => expect(panel.text()).toContain('升级目标版本 Python 分析 v2'))
      expect(panel.text()).not.toContain('固定使用')
      expect(wrapper.get('[data-testid="upgrade-target-note"]').text()).toContain('升级操作成功后才表示新版本已在运行')
      expect(wrapper.find('[data-testid="upgrade-to-2"]').exists()).toBe(false)
    })

    it('keeps a failed upgrade visible with its diagnostic instead of claiming completion', async () => {
      mockWork({
        releaseId: 'release-work-2',
        releaseVersion: 2,
        observedState: 'failed',
        operation: { ...settledOperation, kind: 'restart', state: 'failed', diagnosticCode: 'LW_WORK_TEMPLATE_UPGRADE_TIMEOUT', templateUpgrade: upgradeRecord(1, 2) },
      })
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const status = await vi.waitFor(() => wrapper.get('[data-testid="upgrade-status"]'))
      expect(status.attributes('data-state')).toBe('failed')
      expect(status.text()).toContain('升级没有完成，新版本没有运行')
      expect(status.text()).toContain('LW_WORK_TEMPLATE_UPGRADE_TIMEOUT')
      expect(status.text()).toContain('重试失败的操作')
      expect(status.text()).not.toContain('升级已完成')
      expect(wrapper.get('[data-testid="upgrade-target-note"]').exists()).toBe(true)
    })

    it('calls the upgrade complete only once the instance operation succeeded', async () => {
      mockWork({
        releaseId: 'release-work-2',
        releaseVersion: 2,
        observedState: 'ready',
        operation: { ...settledOperation, kind: 'restart', state: 'succeeded', templateUpgrade: upgradeRecord(1, 2) },
      })
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const status = await vi.waitFor(() => wrapper.get('[data-testid="upgrade-status"]'))
      expect(status.attributes('data-state')).toBe('succeeded')
      expect(status.text()).toContain('升级已完成，Work 正在使用 v2')
      const panel = wrapper.get('[data-testid="template-upgrade"]')
      await vi.waitFor(() => expect(panel.text()).toContain('固定使用 Python 分析 v2'))
      expect(panel.find('[data-testid="upgrade-target-note"]').exists()).toBe(false)
    })

    it('ignores an upgrade that is only in the history while the current operation is an ordinary one', async () => {
      mockWork({ operation: { ...settledOperation, kind: 'restart' } })
      vi.mocked(listEnvironmentOperations).mockResolvedValue({
        data: { items: [mockOperation('failed', { kind: 'restart', acceptedAt: '2026-07-11T11:00:00.000Z', templateUpgrade: upgradeRecord(1, 2) })] },
        error: undefined as never,
      } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const panel = await vi.waitFor(() => wrapper.get('[data-testid="template-upgrade"]'))
      await vi.waitFor(() => expect(listEnvironmentOperations).toHaveBeenCalled())
      await flushPromises()
      expect(panel.find('[data-testid="upgrade-status"]').exists()).toBe(false)
      expect(panel.find('[data-testid="upgrade-target-note"]').exists()).toBe(false)
      await vi.waitFor(() => expect(panel.find('[data-testid="upgrade-to-2"]').exists()).toBe(true))
    })

    it('follows the instance operation when the history answer is newer or arrives later', async () => {
      mockWork({
        releaseId: 'release-work-2',
        releaseVersion: 2,
        observedState: 'updating',
        operation: { ...settledOperation, kind: 'restart', state: 'running', templateUpgrade: upgradeRecord(1, 2) },
      })
      let answerHistory: (value: unknown) => void = () => undefined
      vi.mocked(listEnvironmentOperations).mockImplementation(() => new Promise((resolve) => { answerHistory = resolve }) as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const status = await vi.waitFor(() => wrapper.get('[data-testid="upgrade-status"]'))
      expect(status.text()).toContain('模板升级 v1 → 目标版本 v2')

      // A history answer with a later, different upgrade record must not replace the instance's current fact.
      answerHistory({ data: { items: [mockOperation('succeeded', { kind: 'restart', acceptedAt: '2026-07-11T12:00:00.000Z', templateUpgrade: upgradeRecord(2, 3) })] }, error: undefined })
      await flushPromises()
      const after = wrapper.get('[data-testid="upgrade-status"]')
      expect(after.attributes('data-state')).toBe('running')
      expect(after.text()).toContain('模板升级 v1 → 目标版本 v2')
      expect(after.text()).not.toContain('v3')
    })

    it('keeps upgrade request identities per user and drops them when another user signs in', async () => {
      mockWork()
      vi.mocked(upgradeEnvironmentTemplate).mockResolvedValue({ data: undefined, error: { status: 503, diagnosticCode: 'LW_ENVIRONMENT_LEASE_VERIFICATION_RESPONSE_INVALID', detail: 'invalid', retryable: true } } as never)
      const first = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      await confirmUpgradeTo(first.wrapper, 2)
      expect(Object.keys(window.sessionStorage).some((key) => key.startsWith('labweaver.pending-command:student-1:work-template-upgrade:env-1:'))).toBe(true)
      first.wrapper.unmount()

      signIn('student-2')
      expect(Object.keys(window.sessionStorage).filter((key) => key.startsWith('labweaver.pending-command:'))).toEqual([])
      const second = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      await confirmUpgradeTo(second.wrapper, 2)
      const [byFirst, bySecond] = vi.mocked(upgradeEnvironmentTemplate).mock.calls.map(([options]) => options.headers['Idempotency-Key'])
      expect(bySecond).not.toBe(byFirst)
    })

    it.each([
      ['409 lease not active', 409, 'LW_RUNTIME_LEASE_NOT_ACTIVE'],
      ['409 operation in progress', 409, 'LW_OPERATION_IN_PROGRESS'],
      ['403 permission', 403, 'LW_SCOPE_DENIED'],
    ])('keeps the upgrade identity after %s and resubmits with the same key', async (_label, status, diagnosticCode) => {
      mockWork()
      vi.mocked(upgradeEnvironmentTemplate)
        .mockResolvedValueOnce({ data: undefined, error: { status, diagnosticCode, detail: 'refused', retryable: false } } as never)
        .mockResolvedValueOnce({ data: { environmentId: 'env-1', operationId: 'op-upgrade', revision: 12, statusUrl: '/x' }, error: undefined } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const panel = await confirmUpgradeTo(wrapper, 2)
      expect(panel.get('[data-testid="upgrade-unconfirmed"]').text()).toContain('升级结果待确认')
      expect(Object.keys(window.sessionStorage).filter((key) => key.startsWith('labweaver.pending-command:student-1:work-template-upgrade:'))).toHaveLength(1)

      // These refusals are not retryable, so the user confirms the same target again.
      await confirmUpgradeTo(wrapper, 2)
      const [first, second] = vi.mocked(upgradeEnvironmentTemplate).mock.calls.map(([options]) => options.headers['Idempotency-Key'])
      expect(second).toBe(first)
      // Accepted: the identity is released.
      expect(Object.keys(window.sessionStorage).filter((key) => key.startsWith('labweaver.pending-command:'))).toEqual([])
    })

    it('does not let a late answer for an older upgrade command delete a newer one in the same scope', async () => {
      mockWork()
      let finishA: (value: unknown) => void = () => undefined
      vi.mocked(upgradeEnvironmentTemplate).mockImplementationOnce(() => new Promise((resolve) => { finishA = resolve }) as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      await confirmUpgradeTo(wrapper, 2)
      const storageKey = Object.keys(window.sessionStorage).find((key) => key.startsWith('labweaver.pending-command:student-1:work-template-upgrade:'))!
      const a = JSON.parse(window.sessionStorage.getItem(storageKey)!)
      // Another tab of the same actor replaced the command in this scope with B.
      window.sessionStorage.setItem(storageKey, JSON.stringify({ ...a, idempotencyKey: 'key-b' }))

      finishA({ data: { environmentId: 'env-1', operationId: 'op-a', revision: 12, statusUrl: '/a' }, error: undefined })
      await flushPromises()
      expect(JSON.parse(window.sessionStorage.getItem(storageKey)!).idempotencyKey).toBe('key-b')
    })

    it('never offers an in-place upgrade for VM Work and explains the alternative', async () => {
      mockWork({ runtimeKind: 'virtual_machine' })
      vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
        data: { items: [workRelease(1, { runtimeKind: 'virtual_machine' }), workRelease(2, { runtimeKind: 'virtual_machine' })], nextCursor: null },
        error: undefined as never,
      } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      const panel = await vi.waitFor(() => wrapper.get('[data-testid="template-upgrade"]'))
      expect(panel.get('[data-testid="vm-upgrade-unsupported"]').text()).toContain('虚拟机 Work 不支持在原 Work 内升级模板')
      expect(panel.get('[data-testid="vm-upgrade-unsupported"]').text()).toContain('新建 Work，再自行把需要的数据迁移过去')
      const link = await vi.waitFor(() => panel.get('[data-testid="vm-new-work-link"]'))
      expect(link.text()).toBe('用 Python 分析 v2 新建 Work')
      expect(link.attributes('href')).toBe('/researcher/resources?projectId=project-1&releaseId=release-work-2&releaseVersion=2')
      expect(panel.findAll('button').some((button) => button.text().startsWith('升级到'))).toBe(false)
      expect(wrapper.findAllComponents({ name: 'ConfirmDialog' }).some((dialog) => dialog.props('title') === '升级模板并重启 Work' && dialog.props('open'))).toBe(false)
      expect(upgradeEnvironmentTemplate).not.toHaveBeenCalled()
    })

    it('explains the VM refusal from the server in Chinese before the code', () => {
      const diagnostic = makeDiagnostic('LW_WORK_TEMPLATE_UPGRADE_UNSUPPORTED_FOR_VM', 'vm unsupported', false, { status: 409, requestId: 'req-vm' })
      expect(diagnostic.message).toBe('虚拟机 Work 不支持在原 Work 内升级模板：需要导出/导入流程，当前不可用。')
      expect(diagnostic.nextStep).toBe('如需使用新版本，请用新版本模板新建 Work，再自行把需要的数据迁移过去。')
      expect(diagnostic.requestId).toBe('req-vm')
    })

    it('shows no upgrade panel for a course experiment', async () => {
      mockEnvironmentInstance({ operation: settledOperation })
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' })
      await vi.waitFor(() => expect(wrapper.find('[data-testid="connection-summary"]').exists()).toBe(true))
      expect(wrapper.find('[data-testid="template-upgrade"]').exists()).toBe(false)
    })

    it('asks for a new resource request when starting a Work whose lease is no longer active', async () => {
      mockWork({ desiredState: 'stopped', observedState: 'stopped', operation: { ...settledOperation, kind: 'stop' } })
      vi.mocked(startEnvironment).mockResolvedValue({
        data: undefined,
        error: { status: 409, diagnosticCode: 'LW_RUNTIME_LEASE_NOT_ACTIVE', detail: 'lease not active', requestId: 'req-start', retryable: false },
      } as never)
      const { wrapper } = await mountAt({ environmentId: 'env-1', projectId: 'project-1' }, false, false, '/researcher/environments')
      await vi.waitFor(() => expect((wrapper.find('button[aria-label="启动"]').element as HTMLButtonElement).disabled).toBe(false))
      await wrapper.get('button[aria-label="启动"]').trigger('click')
      await flushPromises()

      const result = wrapper.get('.lifecycle-result')
      expect(result.text()).toContain('资源使用授权已到期、被回收或尚未生效')
      expect(result.text()).toContain('请提交新的资源申请')
      expect(result.findAll('button').some((button) => button.text() === '重试')).toBe(false)
      expect(result.get('[data-testid="lease-new-request-link"]').attributes('href')).toContain('/researcher/resources')
    })
  })
})
