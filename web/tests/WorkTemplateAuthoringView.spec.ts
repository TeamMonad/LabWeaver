import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import WorkTemplateAuthoringView from '@/views/researcher/WorkTemplateAuthoringView.vue'
import {
  appendProjectEnvironmentCandidateDecision,
  createEnvironmentTemplateRelease,
  createProjectAgentRun,
  getActiveProjectLlmPolicy,
  getEnvironmentTemplateRelease,
  getProjectAgentRun,
  getProjectEnvironmentCandidate,
  withdrawEnvironmentTemplateRelease,
} from '@/generated/contracts'

const packageData = {
  id: 'package-1',
  projectId: 'project-1',
  courseId: 'course-1',
  revision: 4,
  files: [],
  retention: {},
  completedAt: '2026-09-08T00:00:00.000Z',
}

const policy = {
  id: 'policy-1',
  projectId: 'project-1',
  courseId: 'course-1',
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

const run = {
  id: 'run-1',
  projectId: 'project-1',
  courseId: 'course-1',
  packageId: 'package-1',
  policyId: 'policy-1',
  policyRevision: 2,
  purpose: { kind: 'authoring', environmentClass: 'work' },
  revision: 1,
  state: 'succeeded',
  tracks: [
    { kind: 'environment', candidateId: 'candidate-1', attempts: [] },
    { kind: 'evaluation', candidateId: null, attempts: [] },
  ],
}

const spec = {
  apiVersion: 'environment.labweaver.io/v1',
  kind: 'EnvironmentSpec',
  class: 'work',
  name: 'generated-work',
  entries: [],
  network: { mode: 'deny_all' },
  resources: { cpuMillicores: 500, memoryBytes: 1024, storageBytes: 4096 },
  retention: { class: 'build_evidence', disposition: 'retain_sanitized_receipt' },
  runtime: {
    kind: 'container',
    provider_binding: 'local-container',
    service_port: 8080,
    build_context: {
      artifactId: 'context-1',
      mediaType: 'application/tar',
      objectVersion: 'version-1',
      sizeBytes: 12,
      storeBinding: 'local-minio',
    },
  },
  security: {
    privilegeEscalationPolicy: 'deny',
    publicExposurePolicy: 'deny',
    rootFilesystemPolicy: 'read_only',
    securityProfileBinding: 'default',
    userPolicy: 'non_root',
  },
}

const imageArtifact = {
  kind: 'container',
  id: 'image-1',
  build_request_id: 'build-1',
  repository: 'registry.local/work',
  digest: 'sha256:image',
}

function makeCandidate(overrides: Record<string, unknown> = {}) {
  return {
    candidate: {
      id: 'candidate-1',
      projectId: 'project-1',
      courseId: 'course-1',
      runId: 'run-1',
      model: 'claude-sonnet',
      policyRevision: 2,
      revision: 3,
      createdAt: '2026-09-08T00:00:00.000Z',
      spec,
    },
    approvals: [],
    trustRevision: 1,
    build: { state: 'succeeded', artifact: imageArtifact },
    imageArtifact,
    ...overrides,
  }
}

const approval = {
  id: 'approval-1',
  actorId: 'teacher-1',
  candidateId: 'candidate-1',
  candidateRevision: 3,
  decidedAt: '2026-09-08T00:01:00.000Z',
  decision: 'approved',
  policyRevision: 2,
  reason: 'reviewed',
  trustRevision: 1,
}

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    appendProjectEnvironmentCandidateDecision: vi.fn(),
    createEnvironmentTemplateRelease: vi.fn(),
    createProjectAgentRun: vi.fn(),
    getActiveProjectLlmPolicy: vi.fn(),
    getEnvironmentTemplateRelease: vi.fn(),
    getProjectEnvironmentCandidate: vi.fn(),
    getProjectAgentRun: vi.fn(),
    withdrawEnvironmentTemplateRelease: vi.fn(),
  }
})

const packageUploadMock = vi.hoisted(() => ({
  loadPackage: vi.fn(),
  initialPackage: {
    id: 'package-1',
    projectId: 'project-1',
    courseId: 'course-1',
    revision: 4,
    files: [],
    retention: {},
    completedAt: '2026-09-08T00:00:00.000Z',
  },
  instance: null as { state: { kind: string; package?: { id: string } } } | null,
}))

vi.mock('@/composables/useProjectProblemPackageUpload', async () => {
  const { reactive } = await import('vue')
  const upload = reactive({
    files: [],
    state: { kind: 'done', package: packageUploadMock.initialPackage },
    addFiles: vi.fn(),
    addDirectoryItems: vi.fn(),
    removeFile: vi.fn(),
    clear: vi.fn(),
    createSession: vi.fn(),
    retry: vi.fn(),
    loadPackage: packageUploadMock.loadPackage,
    formatBytes: (size: number) => `${size} B`,
  })
  packageUploadMock.instance = upload
  return { useProjectProblemPackageUpload: () => upload }
})

describe('WorkTemplateAuthoringView', () => {
  let candidate: ReturnType<typeof makeCandidate>

  beforeEach(() => {
    vi.resetAllMocks()
    if (packageUploadMock.instance) packageUploadMock.instance.state = { kind: 'done', package: packageData }
    packageUploadMock.loadPackage.mockImplementation(async () => {
      if (packageUploadMock.instance) packageUploadMock.instance.state = { kind: 'done', package: packageData }
      return true
    })
    candidate = makeCandidate()
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({ data: policy as never, error: undefined as never })
    vi.mocked(createProjectAgentRun).mockResolvedValue({ data: run as never, error: undefined as never })
    vi.mocked(getProjectEnvironmentCandidate).mockImplementation(async () => ({ data: candidate as never, error: undefined as never }))
    vi.mocked(appendProjectEnvironmentCandidateDecision).mockResolvedValue({ data: approval as never, error: undefined as never })
    vi.mocked(createEnvironmentTemplateRelease).mockResolvedValue({ data: { operationId: 'operation-1', statusUrl: '/operations/operation-1' } as never, error: undefined as never })
    vi.mocked(withdrawEnvironmentTemplateRelease).mockResolvedValue({ data: undefined as never, error: undefined as never })
  })

  async function mountView() {
    const wrapper = mount(WorkTemplateAuthoringView, {
      props: { projectId: 'project-1', courseId: 'course-1' },
      global: { stubs: { RouterLink: true, CandidateBuildTask: true } },
    })
    const runButton = wrapper.get('section[aria-labelledby="run-heading"] > button.filled-button')
    await vi.waitFor(() => expect((runButton.element as HTMLButtonElement).disabled).toBe(false))
    await runButton.trigger('click')
    await vi.waitFor(() => expect(getProjectEnvironmentCandidate).toHaveBeenCalledWith({ path: { projectId: 'project-1', candidateId: 'candidate-1' } }))
    return wrapper
  }

  it('starts a Work AgentRun without an existing environment id', async () => {
    const wrapper = await mountView()

    expect(createProjectAgentRun).toHaveBeenCalledWith(expect.objectContaining({
      path: { projectId: 'project-1' },
      body: expect.objectContaining({
        projectId: 'project-1',
        packageId: 'package-1',
        environmentClass: 'work',
      }),
    }))
    expect(createProjectAgentRun.mock.calls[0][0].body).not.toHaveProperty('environmentId')
    wrapper.unmount()
  })

  it('prevents a second start for the same archived package', async () => {
    const wrapper = await mountView()

    const runButton = wrapper.get('section[aria-labelledby="run-heading"] > button.filled-button')
    expect((runButton.element as HTMLButtonElement).disabled).toBe(true)
    expect(createProjectAgentRun).toHaveBeenCalledTimes(1)
    wrapper.unmount()
  })

  it('only exposes an Environment retry when that track has a failed attempt', async () => {
    const partiallySucceededRun = {
      ...run,
      state: 'partially_succeeded',
      tracks: [
        { kind: 'environment', candidateId: 'candidate-1', attempts: [{ number: 1, state: 'succeeded' }] },
        { kind: 'evaluation', candidateId: null, attempts: [{ number: 1, state: 'failed' }] },
      ],
    }
    vi.mocked(createProjectAgentRun).mockResolvedValue({ data: partiallySucceededRun as never, error: undefined as never })

    const wrapper = await mountView()

    expect(wrapper.text()).not.toContain('重试 Environment 轨道')
    wrapper.unmount()
  })

  it('shows a readable resource approval timeout and keeps the diagnostic code advanced', async () => {
    const failedRun = {
      ...run,
      state: 'failed',
      tracks: [
        {
          kind: 'environment',
          candidateId: null,
          attempts: [{ number: 1, state: 'failed', diagnosticCode: 'LW_TASK_RESOURCE_APPROVAL_TIMEOUT' }],
        },
        { kind: 'evaluation', candidateId: null, attempts: [] },
      ],
    }
    vi.mocked(createProjectAgentRun).mockResolvedValue({ data: failedRun as never, error: undefined as never })

    const wrapper = mount(WorkTemplateAuthoringView, {
      props: { projectId: 'project-1', courseId: 'course-1' },
      global: { stubs: { RouterLink: true, CandidateBuildTask: true } },
    })
    const runButton = wrapper.get('section[aria-labelledby="run-heading"] > button.filled-button')
    await vi.waitFor(() => expect((runButton.element as HTMLButtonElement).disabled).toBe(false))
    await runButton.trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('资源申请未及时获批，请查看申请状态或联系资源管理员；确认失败后可重试原任务。'))

    expect(wrapper.text()).toContain('临时资源回收不等于整体生成成功')
    expect(wrapper.text()).toContain('重试环境候选生成')
    expect(wrapper.text()).not.toContain('模型不可用')
    expect(wrapper.get('details.technical-details').text()).toContain('LW_TASK_RESOURCE_APPROVAL_TIMEOUT')
    wrapper.unmount()
  })

  it('waits for the Environment candidate projection after a transient not-found response', async () => {
    vi.useFakeTimers()
    try {
      vi.mocked(getProjectEnvironmentCandidate)
        .mockResolvedValueOnce({ data: undefined as never, response: { status: 404 } as never, error: { diagnosticCode: 'LW_CANDIDATE_NOT_FOUND', detail: 'candidate projection pending', retryable: false } as never })
        .mockResolvedValueOnce({ data: candidate as never, error: undefined as never })

      const wrapper = await mountView()
      expect(wrapper.text()).toContain('等待环境候选同步')

      await vi.advanceTimersByTimeAsync(3000)
      await vi.waitFor(() => expect(getProjectEnvironmentCandidate).toHaveBeenCalledTimes(2))
      await vi.waitFor(() => expect(wrapper.find('.candidate-summary').exists()).toBe(true))
      wrapper.unmount()
    } finally {
      vi.useRealTimers()
    }
  })

  it('does not apply an in-flight candidate response after the project changes', async () => {
    let resolveCandidate!: (value: unknown) => void
    vi.mocked(getProjectEnvironmentCandidate).mockImplementationOnce(
      () => new Promise((resolve) => { resolveCandidate = resolve as (value: unknown) => void }) as never,
    )

    const wrapper = await mountView()
    await wrapper.setProps({ projectId: 'project-2' })
    resolveCandidate({ data: candidate, error: undefined })
    await vi.waitFor(() => expect(getProjectEnvironmentCandidate).toHaveBeenCalledTimes(1))
    expect(wrapper.find('.candidate-summary').exists()).toBe(false)
    wrapper.unmount()
  })

  it('surfaces a non-projection candidate error without scheduling a retry', async () => {
    vi.useFakeTimers()
    try {
      vi.mocked(getProjectEnvironmentCandidate).mockResolvedValueOnce({
        data: undefined as never,
        error: { diagnosticCode: 'LW_PROJECT_NOT_FOUND', detail: 'project missing', retryable: false } as never,
      })

      const wrapper = await mountView()
      expect(wrapper.text()).toContain('project missing')
      await vi.advanceTimersByTimeAsync(6000)
      expect(getProjectEnvironmentCandidate).toHaveBeenCalledTimes(1)
      wrapper.unmount()
    } finally {
      vi.useRealTimers()
    }
  })

  it('does not retry a candidate-not-found diagnostic from a non-404 response', async () => {
    vi.useFakeTimers()
    try {
      vi.mocked(getProjectEnvironmentCandidate).mockResolvedValueOnce({
        data: undefined as never,
        response: { status: 500 } as never,
        error: { diagnosticCode: 'LW_CANDIDATE_NOT_FOUND', detail: 'projection lookup failed', retryable: true } as never,
      })

      const wrapper = await mountView()
      expect(wrapper.text()).toContain('projection lookup failed')
      await vi.advanceTimersByTimeAsync(6000)
      expect(getProjectEnvironmentCandidate).toHaveBeenCalledTimes(1)
      wrapper.unmount()
    } finally {
      vi.useRealTimers()
    }
  })

  it('keeps approval and release blocked until a verified artifact and review confirmation exist', async () => {
    candidate = makeCandidate({ imageArtifact: null, build: { state: 'requested' } })
    const wrapper = await mountView()
    const approvalButton = wrapper.get('[data-testid="work-template-candidate-approval-form"] button[type="submit"]')

    expect((approvalButton.element as HTMLButtonElement).disabled).toBe(true)
    expect(wrapper.find('[data-testid="work-template-release"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('keeps the release blocked for an existing approval until the candidate is confirmed', async () => {
    candidate = makeCandidate({ approvals: [approval] })
    const wrapper = await mountView()
    const releaseButton = wrapper.get('[data-testid="work-template-release-button"]')

    expect((releaseButton.element as HTMLButtonElement).disabled).toBe(true)
    await wrapper.get('[data-testid="work-template-candidate-confirmation"]').setValue(true)
    expect((releaseButton.element as HTMLButtonElement).disabled).toBe(false)
    wrapper.unmount()
  })

  it('keeps the candidate review context after an approval failure', async () => {
    vi.mocked(appendProjectEnvironmentCandidateDecision).mockResolvedValue({
      data: undefined as never,
      error: { response: { data: { diagnosticCode: 'APPROVAL_FAILED', detail: 'review rejected', retryable: false } } } as never,
    })
    const wrapper = await mountView()
    await wrapper.get('[data-testid="work-template-candidate-confirmation"]').setValue(true)
    await wrapper.get('textarea[required]').setValue('reviewed generated Work candidate')
    await wrapper.get('[data-testid="work-template-candidate-approval-form"]').trigger('submit')

    await vi.waitFor(() => expect(appendProjectEnvironmentCandidateDecision).toHaveBeenCalledTimes(1))
    expect(wrapper.find('[data-testid="work-template-candidate"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="work-template-release"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('publishes the exact approved Work candidate tuple', async () => {
    const wrapper = await mountView()
    await wrapper.get('[data-testid="work-template-candidate-confirmation"]').setValue(true)
    await wrapper.get('textarea[required]').setValue('reviewed generated Work candidate')
    await wrapper.get('[data-testid="work-template-candidate-approval-form"]').trigger('submit')
    await vi.waitFor(() => expect(appendProjectEnvironmentCandidateDecision).toHaveBeenCalledTimes(1))

    await wrapper.get('[data-testid="work-template-release-button"]').trigger('click')
    await vi.waitFor(() => expect(createEnvironmentTemplateRelease).toHaveBeenCalledTimes(1))
    expect(createEnvironmentTemplateRelease).toHaveBeenCalledWith(expect.objectContaining({
      path: { projectId: 'project-1' },
      body: {
        projectId: 'project-1',
        courseId: 'course-1',
        candidateId: 'candidate-1',
        candidateRevision: 3,
        runtimeKind: 'container',
        approvalId: 'approval-1',
      },
      headers: { 'Idempotency-Key': expect.any(String) },
    }))
    wrapper.unmount()
  })

  it('restores the run, candidate approval, and published release from durable route ids', async () => {
    candidate = makeCandidate({ approvals: [approval] })
    vi.mocked(getProjectAgentRun).mockResolvedValue({ data: run as never, error: undefined as never })
    vi.mocked(getEnvironmentTemplateRelease).mockResolvedValue({
      data: { id: 'release-1', version: 2 } as never,
      error: undefined as never,
    })
    const wrapper = mount(WorkTemplateAuthoringView, {
      props: { projectId: 'project-1', courseId: 'course-1', runId: 'run-1', releaseId: 'release-1' },
      global: { stubs: { RouterLink: true, CandidateBuildTask: true } },
    })

    await vi.waitFor(() => expect(getProjectAgentRun).toHaveBeenCalledWith({ path: { projectId: 'project-1', runId: 'run-1' } }))
    await vi.waitFor(() => expect(getProjectEnvironmentCandidate).toHaveBeenCalledWith({ path: { projectId: 'project-1', candidateId: 'candidate-1' } }))
    await vi.waitFor(() => expect(getEnvironmentTemplateRelease).toHaveBeenCalledWith({ path: { projectId: 'project-1', releaseId: 'release-1' } }))
    expect(wrapper.find('[data-testid="work-template-release"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="work-template-release-button"]').exists()).toBe(false)
    expect(wrapper.text()).toContain('release-1')
    wrapper.unmount()
  })

  it('reloads the exact release after a project change while the initial release request is pending', async () => {
    let resolveFirstRelease!: (value: { data: unknown; error: undefined }) => void
    const firstRelease = new Promise<{ data: unknown; error: undefined }>((resolve) => {
      resolveFirstRelease = resolve
    })
    let resolveSecondRelease!: (value: { data: unknown; error: undefined }) => void
    const secondRelease = new Promise<{ data: unknown; error: undefined }>((resolve) => {
      resolveSecondRelease = resolve
    })
    vi.mocked(getEnvironmentTemplateRelease).mockImplementation(async ({ path }) => {
      if (path.releaseId === 'release-1') return firstRelease as never
      return secondRelease as never
    })
    vi.mocked(getProjectAgentRun).mockImplementation(async ({ path }) => ({
      data: {
        ...run,
        id: path.runId,
        projectId: path.projectId,
        tracks: [
          { ...run.tracks[0], candidateId: `candidate-${path.projectId}` },
          run.tracks[1],
        ],
      } as never,
      error: undefined as never,
    }))
    vi.mocked(getProjectEnvironmentCandidate).mockImplementation(async ({ path }) => {
      const candidateId = path.candidateId
      return {
        data: makeCandidate({
          candidate: { ...makeCandidate().candidate, id: candidateId, projectId: path.projectId, runId: `run-${path.projectId}` },
          approvals: [{ ...approval, candidateId }],
        }) as never,
        error: undefined as never,
      }
    })

    const wrapper = mount(WorkTemplateAuthoringView, {
      props: { projectId: 'project-1', courseId: 'course-1', runId: 'run-project-1', releaseId: 'release-1' },
      global: { stubs: { RouterLink: true, CandidateBuildTask: true } },
    })

    await vi.waitFor(() => expect(getEnvironmentTemplateRelease).toHaveBeenCalledWith({ path: { projectId: 'project-1', releaseId: 'release-1' } }))
    await wrapper.setProps({ projectId: 'project-2', runId: 'run-project-2', releaseId: 'release-2' })
    await vi.waitFor(() => expect(getEnvironmentTemplateRelease).toHaveBeenCalledWith({ path: { projectId: 'project-2', releaseId: 'release-2' } }))
    expect(getEnvironmentTemplateRelease).toHaveBeenCalledTimes(2)

    resolveFirstRelease({ data: { id: 'release-1', version: 1 }, error: undefined })
    resolveSecondRelease({ data: { id: 'release-2', version: 2 }, error: undefined })
    await vi.waitFor(() => expect(wrapper.text()).toContain('release-2'))
    expect(wrapper.text()).not.toContain('release-1')
    wrapper.unmount()
  })

  it('requires confirmation before withdrawing a restored Work release', async () => {
    candidate = makeCandidate({ approvals: [approval] })
    vi.mocked(getProjectAgentRun).mockResolvedValue({ data: run as never, error: undefined as never })
    vi.mocked(getEnvironmentTemplateRelease).mockResolvedValue({
      data: { id: 'release-1', version: 2, withdrawal: null } as never,
      error: undefined as never,
    })
    vi.mocked(withdrawEnvironmentTemplateRelease).mockResolvedValue({
      data: {
        releaseId: 'release-1',
        releaseVersion: 2,
        actorId: 'teacher-1',
        reasonCode: 'TEACHER_WITHDRAWN',
        withdrawnAt: '2026-09-12T10:00:00.000Z',
      } as never,
      error: undefined as never,
    })

    const wrapper = mount(WorkTemplateAuthoringView, {
      props: { projectId: 'project-1', courseId: 'course-1', runId: 'run-1', releaseId: 'release-1' },
      global: { stubs: { RouterLink: true, CandidateBuildTask: true } },
    })

    await vi.waitFor(() => expect(getEnvironmentTemplateRelease).toHaveBeenCalledWith({ path: { projectId: 'project-1', releaseId: 'release-1' } }))
    await vi.waitFor(() => expect(getProjectEnvironmentCandidate).toHaveBeenCalledWith({ path: { projectId: 'project-1', candidateId: 'candidate-1' } }))
    const withdrawButton = wrapper.get('[data-testid="work-template-withdraw-release-button"]')
    await withdrawButton.trigger('click')
    const dialog = wrapper.findComponent({ name: 'ConfirmDialog' })
    expect(dialog.props('description')).toContain('已有环境不会自动释放')
    expect(dialog.props('description')).toContain('已建立连接不会因撤回自动断开')
    await dialog.vm.$emit('cancel')
    expect(withdrawEnvironmentTemplateRelease).not.toHaveBeenCalled()

    await withdrawButton.trigger('click')
    await dialog.vm.$emit('confirm')
    await vi.waitFor(() => expect(withdrawEnvironmentTemplateRelease).toHaveBeenCalledTimes(1))
    expect(withdrawEnvironmentTemplateRelease).toHaveBeenCalledWith(expect.objectContaining({
      path: { projectId: 'project-1', releaseId: 'release-1' },
      headers: { 'Idempotency-Key': expect.any(String), 'If-Match': '"rev-2"' },
      body: { reasonCode: 'TEACHER_WITHDRAWN' },
    }))
    expect(wrapper.text()).toContain('Work 模板已撤回')
    expect(wrapper.find('[data-testid="work-template-resource-link"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('restores the archived package for a failed route run and keeps its track retry available', async () => {
    const failedRun = {
      ...run,
      state: 'failed',
      tracks: [
        { kind: 'environment', candidateId: null, attempts: [{ number: 1, state: 'failed', diagnosticCode: 'WORK_TEMPLATE_FAILED' }] },
        { kind: 'evaluation', candidateId: null, attempts: [] },
      ],
    }
    vi.mocked(getProjectAgentRun).mockResolvedValue({ data: failedRun as never, error: undefined as never })
    packageUploadMock.instance!.state = { kind: 'idle' }

    const wrapper = mount(WorkTemplateAuthoringView, {
      props: { projectId: 'project-1', courseId: 'course-1', runId: 'run-1' },
      global: { stubs: { RouterLink: true, CandidateBuildTask: true } },
    })

    await vi.waitFor(() => expect(packageUploadMock.loadPackage).toHaveBeenCalledWith('package-1'))
    expect(wrapper.text()).toContain('重试环境候选生成')

    await wrapper.get('button[aria-label="刷新生成任务"]').trigger('click')
    await vi.waitFor(() => expect(getProjectAgentRun).toHaveBeenCalledTimes(2))
    expect(packageUploadMock.loadPackage).toHaveBeenCalledTimes(1)
    wrapper.unmount()
  })

  it('keeps a route restore error visible and retries the same run before loading its package', async () => {
    const failedRun = {
      ...run,
      state: 'failed',
      tracks: [
        { kind: 'environment', candidateId: null, attempts: [{ number: 1, state: 'failed', diagnosticCode: 'WORK_TEMPLATE_FAILED' }] },
        { kind: 'evaluation', candidateId: null, attempts: [] },
      ],
    }
    vi.mocked(getProjectAgentRun)
      .mockResolvedValueOnce({ data: undefined as never, error: { diagnosticCode: 'PROJECT_RUN_LOAD_FAILED', detail: '任务读取暂时失败', retryable: true } as never })
      .mockResolvedValueOnce({ data: failedRun as never, error: undefined as never })
    packageUploadMock.instance!.state = { kind: 'idle' }

    const wrapper = mount(WorkTemplateAuthoringView, {
      props: { projectId: 'project-1', courseId: 'course-1', runId: 'run-1' },
      global: { stubs: { RouterLink: true, CandidateBuildTask: true } },
    })

    await vi.waitFor(() => expect(wrapper.text()).toContain('任务读取暂时失败'))
    expect(wrapper.find('section[aria-labelledby="run-heading"]').exists()).toBe(true)
    const retryButton = wrapper.findAll('section[aria-labelledby="run-heading"] .diagnostic-banner button').find((button) => button.text() === '重试')
    expect(retryButton).toBeDefined()
    await retryButton!.trigger('click')
    await vi.waitFor(() => expect(getProjectAgentRun).toHaveBeenCalledTimes(2))
    await vi.waitFor(() => expect(packageUploadMock.loadPackage).toHaveBeenCalledWith('package-1'))
    expect(wrapper.text()).toContain('重试环境候选生成')
    expect(createProjectAgentRun).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('retries loading the archived package for the same route run', async () => {
    const failedRun = {
      ...run,
      state: 'failed',
      tracks: [
        { kind: 'environment', candidateId: null, attempts: [{ number: 1, state: 'failed' }] },
        { kind: 'evaluation', candidateId: null, attempts: [] },
      ],
    }
    vi.mocked(getProjectAgentRun).mockResolvedValue({ data: failedRun as never, error: undefined as never })
    packageUploadMock.instance!.state = { kind: 'idle' }
    packageUploadMock.loadPackage
      .mockImplementationOnce(async () => {
        if (packageUploadMock.instance) packageUploadMock.instance.state = {
          kind: 'error',
          diagnostic: { code: 'UPLOAD_PACKAGE_LOAD_FAILED', message: '归档材料包暂时不可用。', retryable: true },
        } as never
        return false
      })

    const wrapper = mount(WorkTemplateAuthoringView, {
      props: { projectId: 'project-1', courseId: 'course-1', runId: 'run-1' },
      global: { stubs: { RouterLink: true, CandidateBuildTask: true } },
    })

    await vi.waitFor(() => expect(wrapper.text()).toContain('归档材料包暂时不可用'))
    const retryButton = wrapper.findAll('.diagnostic-banner button').find((button) => button.text() === '重试')
    expect(retryButton).toBeDefined()
    await retryButton!.trigger('click')
    await vi.waitFor(() => expect(packageUploadMock.loadPackage).toHaveBeenCalledTimes(2))
    expect(wrapper.text()).toContain('重试环境候选生成')
    wrapper.unmount()
  })

  it('rejects a route run returned for another project without loading its package', async () => {
    vi.mocked(getProjectAgentRun).mockResolvedValue({ data: { ...run, projectId: 'project-1' } as never, error: undefined as never })

    const wrapper = mount(WorkTemplateAuthoringView, {
      props: { projectId: 'project-2', courseId: 'course-1', runId: 'run-1' },
      global: { stubs: { RouterLink: true, CandidateBuildTask: true } },
    })

    await vi.waitFor(() => expect(getProjectAgentRun).toHaveBeenCalledWith({ path: { projectId: 'project-2', runId: 'run-1' } }))
    await Promise.resolve()
    expect(packageUploadMock.loadPackage).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('生成任务返回的项目引用已变化')
    wrapper.unmount()
  })
})
