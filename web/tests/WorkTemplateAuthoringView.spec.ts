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
  }
})

vi.mock('@/composables/useProjectProblemPackageUpload', () => ({
  useProjectProblemPackageUpload: () => ({
    files: [],
    state: { kind: 'done', package: packageData },
    addFiles: vi.fn(),
    addDirectoryItems: vi.fn(),
    removeFile: vi.fn(),
    clear: vi.fn(),
    createSession: vi.fn(),
    retry: vi.fn(),
    formatBytes: (size: number) => `${size} B`,
  }),
}))

describe('WorkTemplateAuthoringView', () => {
  let candidate: ReturnType<typeof makeCandidate>

  beforeEach(() => {
    vi.resetAllMocks()
    candidate = makeCandidate()
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({ data: policy as never, error: undefined as never })
    vi.mocked(createProjectAgentRun).mockResolvedValue({ data: run as never, error: undefined as never })
    vi.mocked(getProjectEnvironmentCandidate).mockImplementation(async () => ({ data: candidate as never, error: undefined as never }))
    vi.mocked(appendProjectEnvironmentCandidateDecision).mockResolvedValue({ data: approval as never, error: undefined as never })
    vi.mocked(createEnvironmentTemplateRelease).mockResolvedValue({ data: { operationId: 'operation-1', statusUrl: '/operations/operation-1' } as never, error: undefined as never })
  })

  async function mountView() {
    const wrapper = mount(WorkTemplateAuthoringView, {
      props: { projectId: 'project-1', courseId: 'course-1' },
      global: { stubs: { RouterLink: true } },
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

  it('waits for the Environment candidate projection after a transient not-found response', async () => {
    vi.useFakeTimers()
    try {
      vi.mocked(getProjectEnvironmentCandidate)
        .mockResolvedValueOnce({ data: undefined as never, response: { status: 404 } as never, error: { diagnosticCode: 'LW_CANDIDATE_NOT_FOUND', detail: 'candidate projection pending', retryable: false } as never })
        .mockResolvedValueOnce({ data: candidate as never, error: undefined as never })

      const wrapper = await mountView()
      expect(wrapper.text()).toContain('等待 Environment 候选同步')

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
      global: { stubs: { RouterLink: true } },
    })

    await vi.waitFor(() => expect(getProjectAgentRun).toHaveBeenCalledWith({ path: { projectId: 'project-1', runId: 'run-1' } }))
    await vi.waitFor(() => expect(getProjectEnvironmentCandidate).toHaveBeenCalledWith({ path: { projectId: 'project-1', candidateId: 'candidate-1' } }))
    await vi.waitFor(() => expect(getEnvironmentTemplateRelease).toHaveBeenCalledWith({ path: { projectId: 'project-1', releaseId: 'release-1' } }))
    expect(wrapper.find('[data-testid="work-template-release"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="work-template-release-button"]').exists()).toBe(false)
    expect(wrapper.text()).toContain('release-1')
    wrapper.unmount()
  })
})
