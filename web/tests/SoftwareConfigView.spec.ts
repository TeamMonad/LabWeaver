import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
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
    await wrapper.find('input[placeholder="已归档 ProblemPackage ID"]').setValue('package-1')
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
  })

  it('requires a future approval expiry before approving a Work plan', async () => {
    const approvedRun = { ...run, state: 'running', revision: 2 }
    vi.mocked(approveProjectWorkConfigurationRun).mockResolvedValue({ data: approvedRun as never, error: undefined as never })
    vi.mocked(getProjectAgentRun).mockResolvedValue({ data: approvedRun as never, error: undefined as never })

    const wrapper = mount(SoftwareConfigView, {
      global: { stubs: { RouterLink: true } },
    })

    await wrapper.find('input[placeholder="已归档 ProblemPackage ID"]').setValue('package-1')
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

    await wrapper.find('input[placeholder="已归档 ProblemPackage ID"]').setValue('package-1')
    await wrapper.find('select[required]').setValue('environment-1')
    await wrapper.find('input[type="checkbox"]').setValue(true)
    await wrapper.find('form.config-form').trigger('submit')
    await vi.waitFor(() => expect(wrapper.text()).toContain('重试 Work 配置'))
    await wrapper.get('button.text-button').trigger('click')

    expect(retryProjectAgentRunTrack).toHaveBeenCalledWith(expect.objectContaining({
      path: { projectId: 'project-1', runId: 'run-1', track: 'work_configuration' },
      headers: expect.objectContaining({ 'If-Match': '"rev-1"' }),
    }))
    await vi.waitFor(() => expect(wrapper.text()).toContain('Submitted'))
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

    await wrapper.find('input[placeholder="已归档 ProblemPackage ID"]').setValue('package-1')
    await wrapper.find('select[required]').setValue('environment-1')
    await wrapper.find('input[type="checkbox"]').setValue(true)
    await wrapper.find('form.config-form').trigger('submit')
    await vi.waitFor(() => expect(wrapper.text()).toContain('开始新的 Work 配置任务'))
    expect(wrapper.text()).not.toContain('重试 Work 配置')
    await wrapper.get('button.outlined-button').trigger('click')

    expect((wrapper.find('input[placeholder="已归档 ProblemPackage ID"]').element as HTMLInputElement).value).toBe('')
    expect((wrapper.find('input[type="number"]').element as HTMLInputElement).value).toBe('1')
    expect((wrapper.find('select[required]').element as HTMLSelectElement).value).toBe('')
    expect((wrapper.find('input[type="checkbox"]').element as HTMLInputElement).checked).toBe(false)
  })
})
