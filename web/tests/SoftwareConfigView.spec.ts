import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { reactive } from 'vue'
import { routeLocationKey, routerKey } from 'vue-router'
import SoftwareConfigView from '@/views/researcher/SoftwareConfigView.vue'
import {
  approveProjectWorkConfigurationRun,
  createProjectWorkConfigurationRun,
  getActiveProjectLlmPolicy,
  getProjectAgentRun,
  getProjectWorkConfigurationPlan,
  listEnvironments,
  listProjectAgentRuns,
  listProjects,
  retryProjectAgentRunTrack,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    approveProjectWorkConfigurationRun: vi.fn(),
    createProjectWorkConfigurationRun: vi.fn(),
    getActiveProjectLlmPolicy: vi.fn(),
    getProjectWorkConfigurationPlan: vi.fn(),
    getProjectAgentRun: vi.fn(),
    listEnvironments: vi.fn(),
    listProjectAgentRuns: vi.fn(),
    listProjects: vi.fn(),
    retryProjectAgentRunTrack: vi.fn(),
  }
})

const packageUploadMock = vi.hoisted(() => {
  const packageView = (id: string) => ({
    id,
    projectId: 'project-1',
    courseId: null,
    revision: 1,
    files: [],
    retention: {},
    completedAt: '2026-09-08T00:00:00.000Z',
  })
  return {
    files: [] as Array<{ path: string; sizeBytes: number; status: string; progress: number }>,
    state: { kind: 'done', package: packageView('package-1') } as { kind: string; package: ReturnType<typeof packageView> },
    addFiles: vi.fn(),
    addDirectoryItems: vi.fn(),
    removeFile: vi.fn(),
    clear: vi.fn(function (this: { files: unknown[]; state: unknown }) {
      this.files = []
      this.state = { kind: 'idle' }
    }),
    createSession: vi.fn(),
    loadPackage: vi.fn(),
    retry: vi.fn(),
    formatBytes: (size: number) => `${size} B`,
    setPackage(id: string) {
      this.state = { kind: 'done', package: packageView(id) }
    },
  }
})

vi.mock('@/composables/useProjectProblemPackageUpload', async () => {
  const { reactive } = await import('vue')
  return { useProjectProblemPackageUpload: () => reactive(packageUploadMock) }
})

const project = {
  id: 'project-1',
  name: 'Work project',
  description: 'Software configuration test project',
  ownerActorId: 'actor-1',
  courseId: null,
  revision: 3,
  state: 'active',
  createdAt: '2026-09-08T00:00:00.000Z',
  updatedAt: '2026-09-08T00:00:00.000Z',
}

const policy = {
  id: 'policy-1',
  projectId: 'project-1',
  courseId: null,
  revision: 2,
  activatedAt: '2026-09-08T00:00:00.000Z',
  binding: {
    runtimeBinding: 'local-runtime',
    model: 'claude-sonnet',
    claudeCodeVersion: '2.1.215',
    workerImageSha256: 'a'.repeat(64),
    runtimeConfigSha256: 'b'.repeat(64),
    maxInFlightPerWorker: 1,
  },
  deniedDataClasses: [],
  budget: {
    maxInputTokens: 1000,
    maxOutputTokens: 500,
    maxRequests: 3,
    maxCostMicrousd: 100,
    timeoutMilliseconds: 10000,
    maxTransientRetries: 1,
    maxSchemaRepairs: 1,
  },
  studentContentMode: 'manifest_allowlist_only',
}

const environment = {
  id: 'environment-1',
  projectId: 'project-1',
  courseId: null,
  class: 'work',
  displayLabel: 'Research Work',
  revision: 4,
  runtimeKind: 'container',
  observedState: 'ready',
  updatedAt: '2026-09-08T00:00:00.000Z',
}

const run = {
  id: 'run-1',
  projectId: 'project-1',
  packageId: 'package-1',
  policyId: 'policy-1',
  policyRevision: 2,
  purpose: {
    kind: 'work_configuration',
    actorId: 'actor-1',
    environmentId: 'environment-1',
    environmentRevision: 4,
    runtimeKind: 'container',
  },
  revision: 1,
  state: 'awaiting_approval',
  plan: {
    id: 'plan-1',
    environmentId: 'environment-1',
    environmentRevision: 4,
    revision: 1,
    requiresRestart: false,
    summary: 'Apply the requested packages',
    scriptArtifact: {
      artifactId: 'artifact-1',
      mediaType: 'text/plain',
      objectVersion: 'version-1',
      sizeBytes: 12,
      storeBinding: 'local-minio',
    },
  },
  tracks: [
    { kind: 'work_configuration', candidateId: null, attempts: [] },
  ],
}

const historyTemplate = {
  id: 'history-template-1',
  projectId: 'project-1',
  purpose: { kind: 'authoring', environmentClass: 'work' },
  state: 'failed',
  createdAt: '2026-09-08T10:00:00.000Z',
  updatedAt: '2026-09-08T10:05:00.000Z',
}

const historyConfiguration = {
  id: 'history-config-1',
  projectId: 'project-1',
  purpose: { kind: 'work_configuration', actorId: 'actor-1', environmentId: 'environment-1', environmentRevision: 4, runtimeKind: 'container' },
  state: 'awaiting_approval',
  createdAt: '2026-09-08T11:00:00.000Z',
  updatedAt: '2026-09-08T11:05:00.000Z',
}

const historyExperiment = {
  id: 'history-experiment-1',
  projectId: 'project-1',
  purpose: { kind: 'authoring', environmentClass: 'experiment' },
  state: 'succeeded',
  createdAt: '2026-09-08T09:00:00.000Z',
  updatedAt: '2026-09-08T09:05:00.000Z',
}

const planView = {
  plan: run.plan,
  scriptContent: 'sudo apt-get update',
  verificationScriptContent: null,
}

function localDateTimeInput(timestamp: number) {
  const date = new Date(timestamp)
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

describe('SoftwareConfigView', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-09-08T12:00:00.000Z'))
    vi.resetAllMocks()
    packageUploadMock.files = []
    packageUploadMock.state = {
      kind: 'done',
      package: {
        id: 'package-1', projectId: 'project-1', courseId: null, revision: 1,
        files: [], retention: {}, completedAt: '2026-09-08T00:00:00.000Z',
      },
    }
    vi.mocked(packageUploadMock.loadPackage).mockImplementation(async (id: string) => {
      packageUploadMock.setPackage(id)
      return true
    })
    vi.mocked(listProjects).mockResolvedValue({ data: [project] as never, error: undefined as never })
    vi.mocked(listProjectAgentRuns).mockResolvedValue({ data: { items: [], page: 1, pageSize: 25, hasMore: false } as never, error: undefined as never })
    vi.mocked(listEnvironments).mockResolvedValue({
      data: { items: [environment], nextCursor: null } as never,
      error: undefined as never,
    })
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({ data: policy as never, error: undefined as never })
    vi.mocked(createProjectWorkConfigurationRun).mockResolvedValue({ data: run as never, error: undefined as never })
    vi.mocked(getProjectAgentRun).mockResolvedValue({ data: run as never, error: undefined as never })
    vi.mocked(getProjectWorkConfigurationPlan).mockResolvedValue({ data: planView as never, error: undefined as never })
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('submits an existing Work target and defaults approval to fifteen minutes ahead', async () => {
    const wrapper = mount(SoftwareConfigView, {
      global: { stubs: { RouterLink: true } },
    })

    await vi.waitFor(() => expect(wrapper.text()).toContain('Research Work'))
    expect(wrapper.get('.project-summary .state-chip').text()).toBe('可用')
    await wrapper.find('select[required]').setValue('environment-1')
    await wrapper.find('input[type="checkbox"]').setValue(true)
    await wrapper.find('form.config-form').trigger('submit')

    await vi.waitFor(() => expect(wrapper.text()).toContain('Work 配置计划审核'))
    expect(createProjectWorkConfigurationRun).toHaveBeenCalledWith(expect.objectContaining({
      path: { projectId: 'project-1' },
      body: expect.objectContaining({
        projectId: 'project-1',
        environmentId: 'environment-1',
        environmentRevision: 4,
        packageId: 'package-1',
      }),
    }))

    const expiry = wrapper.find('input[type="datetime-local"]')
    expect(expiry.exists()).toBe(true)
    const expected = new Date(Date.now() + 15 * 60 * 1000)
    const pad = (value: number) => String(value).padStart(2, '0')
    expect((expiry.element as HTMLInputElement).value).toBe(
      `${expected.getFullYear()}-${pad(expected.getMonth() + 1)}-${pad(expected.getDate())}T${pad(expected.getHours())}:${pad(expected.getMinutes())}`,
    )
    expect(wrapper.text()).toContain('Apply the requested packages')
    expect(wrapper.text()).toContain('sudo apt-get update')
    expect((wrapper.get('form.config-form button[type="submit"]').element as HTMLButtonElement).disabled).toBe(true)
  })

  it('restores a route-selected material package without exposing an ID form field', async () => {
    packageUploadMock.state = { kind: 'idle' }
    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: { query: { projectId: 'project-1', packageId: 'published-package' } },
          [routerKey as symbol]: { replace: vi.fn() },
        },
      },
    })

    await vi.waitFor(() => expect(packageUploadMock.loadPackage).toHaveBeenCalledWith('published-package'))
    expect(wrapper.text()).toContain('材料包已准备')
    expect(wrapper.text()).toContain('材料包版本：1')
    expect(wrapper.find('input[aria-label="材料包 ID"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('restores the route package when switching from template to configuration mode', async () => {
    packageUploadMock.state = { kind: 'idle' }
    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: { query: { projectId: 'project-1', packageId: 'published-package', mode: 'template' } },
          [routerKey as symbol]: { replace: vi.fn() },
        },
      },
    })

    await vi.waitFor(() => expect(packageUploadMock.loadPackage).not.toHaveBeenCalled())
    const configurationButton = wrapper.find('button.mode-switch__button')
    expect(configurationButton.text()).toContain('配置')
    await configurationButton.trigger('click')

    await vi.waitFor(() => expect(packageUploadMock.loadPackage).toHaveBeenCalledWith('published-package'))
    expect(wrapper.text()).toContain('材料包已准备')
    wrapper.unmount()
  })

  it('requires a future approval expiry before approving a Work plan', async () => {
    const approvedRun = { ...run, state: 'running', revision: 2 }
    vi.mocked(approveProjectWorkConfigurationRun).mockResolvedValue({ data: approvedRun as never, error: undefined as never })
    vi.mocked(getProjectAgentRun).mockResolvedValue({ data: approvedRun as never, error: undefined as never })

    const wrapper = mount(SoftwareConfigView, {
      global: { stubs: { RouterLink: true } },
    })

    await wrapper.find('select[required]').setValue('environment-1')
    await wrapper.find('input[type="checkbox"]').setValue(true)
    await wrapper.find('form.config-form').trigger('submit')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Work 配置计划审核'))

    await wrapper.find('textarea[required]').setValue('reviewed the requested Work changes')
    const expiry = wrapper.get('input[type="datetime-local"]')
    const approveButton = wrapper.get('form.approval-form button[type="submit"]')
    const pastExpiry = localDateTimeInput(Date.now() - 60 * 1000)
    const futureExpiry = localDateTimeInput(Date.now() + 30 * 60 * 1000)
    await expiry.setValue(pastExpiry)
    expect((approveButton.element as HTMLButtonElement).disabled).toBe(true)

    await expiry.setValue(futureExpiry)
    expect((approveButton.element as HTMLButtonElement).disabled).toBe(false)
    await wrapper.get('form.approval-form').trigger('submit')

    await vi.waitFor(() => expect(approveProjectWorkConfigurationRun).toHaveBeenCalledWith(expect.objectContaining({
      path: { projectId: 'project-1', runId: 'run-1' },
      body: expect.objectContaining({
        expectedRunRevision: 1,
        expectedPlanRevision: 1,
        environmentRevision: 4,
        expiresAt: new Date(futureExpiry).toISOString(),
        reason: 'reviewed the requested Work changes',
        restartConfirmed: false,
      }),
    })))
  })

  it('retries a failed Work track only when no immutable plan exists', async () => {
    const failedRun = {
      ...run,
      state: 'failed',
      plan: null,
      tracks: [
        { kind: 'work_configuration', candidateId: null, attempts: [{ number: 1, state: 'failed', diagnosticCode: 'WORK_CONFIG_FAILED' }] },
      ],
    }
    const retriedRun = { ...failedRun, state: 'requested', revision: 2 }
    vi.mocked(createProjectWorkConfigurationRun).mockResolvedValue({ data: failedRun as never, error: undefined as never })
    vi.mocked(retryProjectAgentRunTrack).mockResolvedValue({ data: retriedRun as never, error: undefined as never })

    const wrapper = mount(SoftwareConfigView, {
      global: { stubs: { RouterLink: true } },
    })

    await wrapper.find('select[required]').setValue('environment-1')
    await wrapper.find('input[type="checkbox"]').setValue(true)
    await wrapper.find('form.config-form').trigger('submit')
    await vi.waitFor(() => expect(wrapper.text()).toContain('重试 Work 配置'))
    await wrapper.findAll('button.text-button').find((button) => button.text() === '重试 Work 配置')!.trigger('click')

    expect(retryProjectAgentRunTrack).toHaveBeenCalledWith(expect.objectContaining({
      path: { projectId: 'project-1', runId: 'run-1', track: 'work_configuration' },
      headers: expect.objectContaining({ 'If-Match': '"rev-1"' }),
    }))
    await vi.waitFor(() => expect(wrapper.text()).toContain('已提交'))
  })

  it('only exposes retry actions for tracks whose latest attempt failed', async () => {
    const partiallySucceededRun = {
      ...run,
      state: 'partially_succeeded',
      plan: null,
      tracks: [
        { kind: 'work_configuration', candidateId: null, attempts: [{ number: 1, state: 'failed', diagnosticCode: 'WORK_CONFIG_FAILED' }] },
        { kind: 'environment', candidateId: 'candidate-1', attempts: [{ number: 1, state: 'succeeded' }] },
      ],
    }
    vi.mocked(createProjectWorkConfigurationRun).mockResolvedValue({ data: partiallySucceededRun as never, error: undefined as never })

    const wrapper = mount(SoftwareConfigView, {
      global: { stubs: { RouterLink: true } },
    })

    await wrapper.find('select[required]').setValue('environment-1')
    await wrapper.find('input[type="checkbox"]').setValue(true)
    await wrapper.find('form.config-form').trigger('submit')
    await vi.waitFor(() => expect(wrapper.text()).toContain('重试 Work 配置'))

    expect(wrapper.text()).not.toContain('重试 Environment')
  })

  it('requires a new Work configuration task after a failed run has an immutable plan', async () => {
    const failedRun = {
      ...run,
      state: 'failed',
      tracks: [
        { kind: 'work_configuration', candidateId: null, attempts: [{ number: 1, state: 'succeeded' }] },
      ],
    }
    vi.mocked(createProjectWorkConfigurationRun).mockResolvedValue({ data: failedRun as never, error: undefined as never })

    const wrapper = mount(SoftwareConfigView, {
      global: { stubs: { RouterLink: true } },
    })

    await wrapper.find('select[required]').setValue('environment-1')
    await wrapper.find('input[type="checkbox"]').setValue(true)
    await wrapper.find('form.config-form').trigger('submit')
    await vi.waitFor(() => expect(wrapper.text()).toContain('开始新的 Work 配置任务'))
    expect(wrapper.text()).not.toContain('重试 Work 配置')
    await wrapper.findAll('button.outlined-button').find((button) => button.text() === '开始新的 Work 配置任务')!.trigger('click')

    expect(packageUploadMock.clear).toHaveBeenCalled()
    expect((wrapper.find('select[required]').element as HTMLSelectElement).value).toBe('')
    expect((wrapper.find('input[type="checkbox"]').element as HTMLInputElement).checked).toBe(false)

    ;(wrapper.vm as unknown as { packageUpload: typeof packageUploadMock }).packageUpload.setPackage('package-2')
    await wrapper.vm.$nextTick()
    await wrapper.find('select[required]').setValue('environment-1')
    await wrapper.find('input[type="checkbox"]').setValue(true)
    await wrapper.find('form.config-form').trigger('submit')
    await vi.waitFor(() => expect(createProjectWorkConfigurationRun).toHaveBeenCalledTimes(2))
  })

  it('restores the configuration run and approval plan from the route without creating a run', async () => {
    const replace = vi.fn()
    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: { query: { projectId: 'project-1', runId: 'run-1' } },
          [routerKey as symbol]: { replace },
        },
      },
    })

    await vi.waitFor(() => expect(wrapper.text()).toContain('Work 配置计划审核'))
    expect(getProjectAgentRun).toHaveBeenCalledWith({ path: { projectId: 'project-1', runId: 'run-1' } })
    expect(createProjectWorkConfigurationRun).not.toHaveBeenCalled()
    expect((wrapper.find('select[required]').element as HTMLSelectElement).value).toBe('environment-1')
    expect(replace.mock.calls.some(([location]) => location.query?.runId === 'run-1')).toBe(true)
    wrapper.unmount()
  })

  it('persists the accepted configuration run ID in the route', async () => {
    const replace = vi.fn()
    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: { query: { projectId: 'project-1' } },
          [routerKey as symbol]: { replace },
        },
      },
    })

    await wrapper.find('select[required]').setValue('environment-1')
    await wrapper.find('input[type="checkbox"]').setValue(true)
    await wrapper.find('form.config-form').trigger('submit')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Work 配置计划审核'))

    expect(replace.mock.calls.some(([location]) => location.query?.runId === 'run-1')).toBe(true)
    wrapper.unmount()
  })

  it('rejects a route run with an authoring purpose instead of treating it as configuration', async () => {
    const wrongPurposeRun = { ...run, purpose: { kind: 'authoring', environmentClass: 'work' } }
    vi.mocked(getProjectAgentRun).mockResolvedValue({ data: wrongPurposeRun as never, error: undefined as never })

    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: { query: { projectId: 'project-1', runId: 'run-1' } },
          [routerKey as symbol]: { replace: vi.fn() },
        },
      },
    })

    await vi.waitFor(() => expect(wrapper.text()).toContain('当前任务不是 Work 配置任务'))
    expect(wrapper.text()).not.toContain('Work 配置计划审核')
    expect(createProjectWorkConfigurationRun).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('rejects a route run whose Work environment is outside the current project inventory', async () => {
    const wrongEnvironmentRun = {
      ...run,
      purpose: { ...run.purpose, environmentId: 'environment-other' },
    }
    vi.mocked(getProjectAgentRun).mockResolvedValue({ data: wrongEnvironmentRun as never, error: undefined as never })

    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: { query: { projectId: 'project-1', runId: 'run-1' } },
          [routerKey as symbol]: { replace: vi.fn() },
        },
      },
    })

    await vi.waitFor(() => expect(wrapper.text()).toContain('绑定的 Work 环境不在当前项目中'))
    expect(wrapper.text()).not.toContain('Work 配置计划审核')
    expect(getProjectWorkConfigurationPlan).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('does not let a late approval plan response overwrite a switched project', async () => {
    const project2 = { ...project, id: 'project-2', name: 'Second Work project' }
    vi.mocked(listProjects).mockResolvedValue({ data: [project, project2] as never, error: undefined as never })
    let resolvePlan!: (value: { data: typeof planView; error: never }) => void
    vi.mocked(getProjectWorkConfigurationPlan).mockReturnValue(new Promise((resolve) => { resolvePlan = resolve }) as never)

    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: { query: { projectId: 'project-1', runId: 'run-1' } },
          [routerKey as symbol]: { replace: vi.fn() },
        },
      },
    })

    await vi.waitFor(() => expect(getProjectWorkConfigurationPlan).toHaveBeenCalledTimes(1))
    await wrapper.find('.project-strip select').setValue('project-2')
    resolvePlan({ data: planView, error: undefined as never })
    await Promise.resolve()

    expect((wrapper.vm as unknown as { plan: { kind: string } }).plan.kind).toBe('idle')
    expect(wrapper.text()).not.toContain('Work 配置计划审核')
    wrapper.unmount()
  })

  it('does not let a late policy response overwrite the template mode', async () => {
    let resolvePolicy!: (value: { data: typeof policy; error: never }) => void
    vi.mocked(getActiveProjectLlmPolicy).mockReturnValueOnce(new Promise((resolve) => { resolvePolicy = resolve }) as never)
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({ data: policy as never, error: undefined as never })

    const wrapper = mount(SoftwareConfigView, {
      global: { stubs: { RouterLink: true } },
    })

    await vi.waitFor(() => expect(getActiveProjectLlmPolicy).toHaveBeenCalledTimes(1))
    await wrapper.get('button.mode-switch__button:nth-of-type(2)').trigger('click')
    resolvePolicy({ data: policy, error: undefined as never })
    await Promise.resolve()

    expect((wrapper.vm as unknown as { policy: { kind: string } }).policy.kind).toBe('idle')
    wrapper.unmount()
  })

  it('lists project task history and opens a Work template run with its mode', async () => {
    vi.mocked(listProjectAgentRuns).mockResolvedValue({
      data: { items: [historyExperiment, historyTemplate, historyConfiguration], page: 1, pageSize: 25, hasMore: false } as never,
      error: undefined as never,
    })
    const replace = vi.fn()
    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: { query: { projectId: 'project-1' } },
          [routerKey as symbol]: { replace },
        },
      },
    })

    await vi.waitFor(() => expect(wrapper.text()).toContain('Work 模板生成'))
    expect(wrapper.text()).toContain('生成失败')
    expect(wrapper.text()).not.toContain('实验候选生成')
    const openTemplate = wrapper.findAll('button').find((button) => button.text() === '打开任务')
    expect(openTemplate).toBeDefined()
    await openTemplate!.trigger('click')

    expect(replace).toHaveBeenCalledWith(expect.objectContaining({
      query: expect.objectContaining({ projectId: 'project-1', mode: 'template', runId: 'history-template-1', releaseId: undefined }),
    }))
    wrapper.unmount()
  })

  it('follows route mode changes and preserves the selected history run through back and forward navigation', async () => {
    const routeState = reactive({ query: { projectId: 'project-1', runId: 'run-1' } as Record<string, string> })
    const replace = vi.fn()
    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: routeState,
          [routerKey as symbol]: { replace },
        },
      },
    })

    await vi.waitFor(() => expect(wrapper.text()).toContain('Work 配置计划审核'))
    routeState.query = { projectId: 'project-1', mode: 'template', runId: 'template-run-1' }
    await vi.waitFor(() => expect(wrapper.get('button.mode-switch__button:nth-of-type(2)').attributes('aria-pressed')).toBe('true'))
    expect((wrapper.vm as unknown as { templateRouteRunId: string | null }).templateRouteRunId).toBe('template-run-1')

    routeState.query = { projectId: 'project-1', runId: 'run-1' }
    await vi.waitFor(() => expect(wrapper.get('button.mode-switch__button:nth-of-type(1)').attributes('aria-pressed')).toBe('true'))
    expect((wrapper.vm as unknown as { configurationRouteRunId: string | null }).configurationRouteRunId).toBe('run-1')
    expect(replace).not.toHaveBeenCalledWith(expect.objectContaining({ query: expect.objectContaining({ runId: undefined }) }))
    wrapper.unmount()
  })

  it('ignores a late history page after switching projects', async () => {
    const project2 = { ...project, id: 'project-2', name: 'Second Work project' }
    vi.mocked(listProjects).mockResolvedValue({ data: [project, project2] as never, error: undefined as never })
    let resolveFirst!: (value: unknown) => void
    vi.mocked(listProjectAgentRuns).mockImplementation((options) => {
      const projectId = (options as { path: { projectId: string } }).path.projectId
      if (projectId === 'project-1') return new Promise((resolve) => { resolveFirst = resolve }) as never
      return Promise.resolve({ data: { items: [historyConfiguration], page: 1, pageSize: 25, hasMore: false }, error: undefined }) as never
    })

    const wrapper = mount(SoftwareConfigView, {
      global: { stubs: { RouterLink: true } },
    })
    await vi.waitFor(() => expect(listProjectAgentRuns).toHaveBeenCalledWith(expect.objectContaining({ path: { projectId: 'project-1' } })))
    await wrapper.find('.project-strip select').setValue('project-2')
    resolveFirst({ data: { items: [historyTemplate], page: 1, pageSize: 25, hasMore: false }, error: undefined })
    await Promise.resolve()

    expect(wrapper.text()).not.toContain('Work 模板生成')
    wrapper.unmount()
  })

  it('ignores a late route-run response after the project changes', async () => {
    const project2 = { ...project, id: 'project-2', name: 'Second Work project' }
    vi.mocked(listProjects).mockResolvedValue({ data: [project, project2] as never, error: undefined as never })
    let resolveRun!: (value: { data: typeof run; error: never }) => void
    vi.mocked(getProjectAgentRun).mockReturnValue(new Promise((resolve) => { resolveRun = resolve }) as never)

    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: { query: { projectId: 'project-1', runId: 'run-1' } },
          [routerKey as symbol]: { replace: vi.fn() },
        },
      },
    })

    await vi.waitFor(() => expect(getProjectAgentRun).toHaveBeenCalledTimes(1))
    await wrapper.find('.project-strip select').setValue('project-2')
    resolveRun({ data: run, error: undefined as never })
    await Promise.resolve()

    expect(getProjectAgentRun).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).not.toContain('Work 配置计划审核')
    wrapper.unmount()
  })

  it('keeps a route project while the shared project list is loading', async () => {
    let resolveProjects: ((value: { data: typeof project[]; error: never }) => void) | undefined
    const projectsResponse = new Promise<{ data: typeof project[]; error: never }>((resolve) => {
      resolveProjects = resolve
    })
    vi.mocked(listProjects).mockReturnValue(projectsResponse as never)
    const replace = vi.fn()
    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: { query: { projectId: project.id } },
          [routerKey as symbol]: { replace },
        },
      },
    })

    await wrapper.get('button.mode-switch__button:nth-of-type(2)').trigger('click')
    await Promise.resolve()
    expect(replace.mock.calls).not.toEqual(expect.arrayContaining([
      [expect.objectContaining({ query: expect.objectContaining({ projectId: undefined }) })],
    ]))

    resolveProjects?.({ data: [project], error: undefined as never })
    await vi.waitFor(() => expect((wrapper.find('select').element as HTMLSelectElement).value).toBe(project.id))
  })

  it('replaces an invalid route project with a project returned by the list', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [project] as never, error: undefined as never })
    const replace = vi.fn()
    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: { query: { projectId: 'missing-project' } },
          [routerKey as symbol]: { replace },
        },
      },
    })

    await vi.waitFor(() => expect((wrapper.find('select').element as HTMLSelectElement).value).toBe(project.id))
    expect(replace.mock.calls.some(([location]) => location.query?.projectId === project.id)).toBe(true)
    expect(replace.mock.calls.some(([location]) => location.query?.projectId === 'missing-project')).toBe(false)
  })

  it('clears a route project when the confirmed project list is empty', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [] as never, error: undefined as never })
    const replace = vi.fn()
    const wrapper = mount(SoftwareConfigView, {
      global: {
        stubs: { RouterLink: true },
        provide: {
          [routeLocationKey as symbol]: { query: { projectId: 'missing-project' } },
          [routerKey as symbol]: { replace },
        },
      },
    })

    await vi.waitFor(() => expect((wrapper.find('select').element as HTMLSelectElement).value).toBe(''))
    expect(replace.mock.calls.some(([location]) => location.query?.projectId === undefined)).toBe(true)
  })
})
