import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { routeLocationKey, routerKey } from 'vue-router'
import SoftwareConfigView from '@/views/researcher/SoftwareConfigView.vue'
import {
  approveProjectWorkConfigurationRun,
  createProjectWorkConfigurationRun,
  getActiveProjectLlmPolicy,
  getProjectAgentRun,
  getProjectWorkConfigurationPlan,
  listEnvironments,
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
    vi.mocked(listProjects).mockResolvedValue({ data: [project] as never, error: undefined as never })
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
