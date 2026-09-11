import { beforeEach, describe, expect, it, vi } from 'vitest'
import { ref } from 'vue'
import { useProjectAuthoringApproval } from '@/composables/useProjectAuthoringApproval'
import {
  completeProjectAuthoringApproval,
  getProjectAgentRun,
  getProjectAuthoringApproval,
  getProjectEnvironmentCandidate,
  getProjectEvaluationCandidate,
  getProjectProblemPackage,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    completeProjectAuthoringApproval: vi.fn(),
    getProjectAgentRun: vi.fn(),
    getProjectAuthoringApproval: vi.fn(),
    getProjectEnvironmentCandidate: vi.fn(),
    getProjectEvaluationCandidate: vi.fn(),
    getProjectProblemPackage: vi.fn(),
  }
})

function makeRun() {
  return {
    id: 'run-1',
    projectId: 'project-1',
    courseId: 'course-1',
    packageId: 'pkg-1',
    policyId: 'policy-1',
    policyRevision: 1,
    purpose: { kind: 'authoring', environmentClass: 'experiment' },
    revision: 2,
    state: 'succeeded',
    tracks: [
      { kind: 'environment', candidateId: 'env-candidate-1', attempts: [] },
      { kind: 'evaluation', candidateId: 'eval-candidate-1', attempts: [] },
    ],
  }
}

function makeEnvironmentCandidate() {
  return {
    candidate: {
      id: 'env-candidate-1',
      projectId: 'project-1',
      courseId: 'course-1',
      runId: 'run-1',
      model: 'claude-sonnet-4-5',
      policyRevision: 1,
      revision: 3,
      createdAt: '2026-07-16T08:00:00.000Z',
      spec: {},
    },
    approvals: [],
    trustRevision: 1,
    build: {
      state: 'succeeded',
      artifact: {
        kind: 'container',
        id: 'build-only-image',
        build_request_id: 'build-only',
        repository: 'registry.labweaver.local/build-only',
        digest: 'sha256:build-only',
      },
    },
    imageArtifact: {
      kind: 'container',
      id: 'image-1',
      build_request_id: 'build-1',
      repository: 'registry.labweaver.local/candidate-1',
      digest: 'sha256:image',
    },
  }
}

function makeEvaluationCandidate() {
  return {
    candidate: {
      id: 'eval-candidate-1',
      projectId: 'project-1',
      courseId: 'course-1',
      runId: 'run-1',
      model: 'claude-sonnet-4-5',
      policyRevision: 1,
      revision: 4,
      createdAt: '2026-07-16T08:00:00.000Z',
      spec: {},
    },
    approvals: [],
    trustRevision: 1,
  }
}

function makeProblemPackage() {
  return {
    id: 'pkg-1',
    projectId: 'project-1',
    courseId: 'course-1',
    revision: 5,
    files: [],
    retention: {},
    completedAt: '2026-07-16T08:00:00.000Z',
  }
}

function makeApproval() {
  return {
    id: 'approval-1',
    projectId: 'project-1',
    courseId: 'course-1',
    packageId: 'pkg-1',
    packageRevision: 5,
    environmentCandidateId: 'env-candidate-1',
    environmentCandidateRevision: 3,
    evaluationCandidateId: 'eval-candidate-1',
    evaluationCandidateRevision: 4,
    imageArtifact: makeEnvironmentCandidate().imageArtifact,
    evaluationRuntimeIdentity: { providerBinding: 'evaluation-v1', runnerImage: 'runner@sha256:image' },
    actorId: 'teacher-1',
    approvedAt: '2026-07-16T09:00:00.000Z',
    reason: 'reviewed',
    revision: 1,
  }
}

function makePublicationStatus(status: 'pending' | 'publishing' | 'ready' | 'failed') {
  return {
    approval: makeApproval(),
    status,
    diagnosticCode: status === 'failed' ? 'AUTHORING_PUBLICATION_FAILED' : null,
    environmentReleaseId: status === 'ready' ? 'environment-release-1' : null,
    evaluationReleaseId: status === 'ready' ? 'evaluation-release-1' : null,
    evaluationReleaseRevision: status === 'ready' ? 1 : null,
    revision: 1,
    updatedAt: '2026-07-16T09:00:00.000Z',
  }
}

describe('useProjectAuthoringApproval', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    vi.mocked(getProjectAgentRun).mockResolvedValue({ data: makeRun() as never, error: undefined as never })
    vi.mocked(getProjectEnvironmentCandidate).mockResolvedValue({ data: makeEnvironmentCandidate() as never, error: undefined as never })
    vi.mocked(getProjectEvaluationCandidate).mockResolvedValue({ data: makeEvaluationCandidate() as never, error: undefined as never })
    vi.mocked(getProjectProblemPackage).mockResolvedValue({ data: makeProblemPackage() as never, error: undefined as never })
  })

  it('loads the project-scoped run, candidates, and package', async () => {
    const approval = useProjectAuthoringApproval(ref<string | null>('project-1'), ref<string | undefined>('run-1'))

    await vi.waitFor(() => expect(approval.problemPackage.kind).toBe('success'))
    expect(approval.run.kind).toBe('success')
    expect(approval.environmentCandidate.kind).toBe('success')
    expect(approval.evaluationCandidate.kind).toBe('success')
    expect(getProjectAgentRun).toHaveBeenCalledWith({ path: { projectId: 'project-1', runId: 'run-1' } })
    expect(getProjectEnvironmentCandidate).toHaveBeenCalledWith({ path: { projectId: 'project-1', candidateId: 'env-candidate-1' } })
    expect(getProjectEvaluationCandidate).toHaveBeenCalledWith({ path: { projectId: 'project-1', candidateId: 'eval-candidate-1' } })
    expect(getProjectProblemPackage).toHaveBeenCalledWith({ path: { projectId: 'project-1', packageId: 'pkg-1' } })
    expect(approval.canApprove).toBe(true)
  })

  it('waits for a projected Environment candidate after a transient not-found response', async () => {
    vi.useFakeTimers()
    try {
      vi.mocked(getProjectEnvironmentCandidate)
        .mockResolvedValueOnce({ data: undefined as never, response: { status: 404 } as never, error: { diagnosticCode: 'LW_CANDIDATE_NOT_FOUND', detail: 'candidate projection pending', retryable: false } as never })
        .mockResolvedValueOnce({ data: makeEnvironmentCandidate() as never, error: undefined as never })

      const approval = useProjectAuthoringApproval(ref<string | null>('project-1'), ref<string | undefined>('run-1'))
      await vi.waitFor(() => expect(getProjectEnvironmentCandidate).toHaveBeenCalledTimes(1))
      expect(approval.environmentCandidate.kind).toBe('loading')

      await vi.advanceTimersByTimeAsync(3000)
      await vi.waitFor(() => expect(getProjectEnvironmentCandidate).toHaveBeenCalledTimes(2))
      await vi.waitFor(() => expect(approval.environmentCandidate.kind).toBe('success'))
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

      const approval = useProjectAuthoringApproval(ref<string | null>('project-1'), ref<string | undefined>('run-1'))
      await vi.waitFor(() => expect(approval.environmentCandidate.kind).toBe('error'))
      await vi.advanceTimersByTimeAsync(6000)
      expect(getProjectEnvironmentCandidate).toHaveBeenCalledTimes(1)
      expect(approval.environmentCandidate.kind).toBe('error')
    } finally {
      vi.useRealTimers()
    }
  })

  it('completes one project-scoped approval with the exact candidate and artifact identities', async () => {
    vi.mocked(completeProjectAuthoringApproval).mockResolvedValue({ data: makeApproval() as never, error: undefined as never })
    const approval = useProjectAuthoringApproval(ref<string | null>('project-1'), ref<string | undefined>('run-1'))
    await vi.waitFor(() => expect(approval.canApprove).toBe(true))

    await expect(approval.complete('  reviewed package and both candidates  ')).resolves.toBe(true)
    expect(approval.canApprove).toBe(false)
    await expect(approval.complete('reviewed package and both candidates')).resolves.toBe(false)
    expect(completeProjectAuthoringApproval).toHaveBeenCalledTimes(1)
    expect(completeProjectAuthoringApproval).toHaveBeenCalledWith(expect.objectContaining({
      path: { projectId: 'project-1' },
      body: expect.objectContaining({
        projectId: 'project-1',
        packageId: 'pkg-1',
        packageRevision: 5,
        environmentCandidateId: 'env-candidate-1',
        environmentCandidateRevision: 3,
        evaluationCandidateId: 'eval-candidate-1',
        evaluationCandidateRevision: 4,
        imageArtifact: expect.objectContaining({ id: 'image-1', digest: 'sha256:image' }),
        reason: 'reviewed package and both candidates',
      }),
    }))
    expect(approval.approval.kind).toBe('success')
    const firstHeaders = vi.mocked(completeProjectAuthoringApproval).mock.calls[0][0].headers
    expect(firstHeaders?.['Idempotency-Key']).toEqual(expect.any(String))
  })

  it('keeps the loaded review context visible when completion conflicts', async () => {
    vi.mocked(completeProjectAuthoringApproval)
      .mockResolvedValueOnce({
        data: undefined as never,
        error: { response: { data: { diagnosticCode: 'REVISION_CONFLICT', detail: 'stale review', retryable: false } } } as never,
      })
      .mockResolvedValueOnce({ data: makeApproval() as never, error: undefined as never })
    const approval = useProjectAuthoringApproval(ref<string | null>('project-1'), ref<string | undefined>('run-1'))
    await vi.waitFor(() => expect(approval.canApprove).toBe(true))

    await expect(approval.complete('reviewed')).resolves.toBe(false)
    expect(approval.environmentCandidate.kind).toBe('success')
    expect(approval.evaluationCandidate.kind).toBe('success')
    expect(approval.problemPackage.kind).toBe('success')
    expect(approval.approval.kind).toBe('error')
    if (approval.approval.kind === 'error') expect(approval.approval.diagnostic.code).toBe('REVISION_CONFLICT')

    await expect(approval.complete('reviewed')).resolves.toBe(true)
    expect(completeProjectAuthoringApproval).toHaveBeenCalledTimes(2)
    const firstKey = vi.mocked(completeProjectAuthoringApproval).mock.calls[0][0].headers?.['Idempotency-Key']
    const retryKey = vi.mocked(completeProjectAuthoringApproval).mock.calls[1][0].headers?.['Idempotency-Key']
    expect(retryKey).toBe(firstKey)
  })

  it('loads the authoritative publication status from a reloadable approval id', async () => {
    vi.mocked(getProjectAuthoringApproval).mockResolvedValue({
      data: makePublicationStatus('pending') as never,
      error: undefined as never,
    })
    const approval = useProjectAuthoringApproval(
      ref<string | null>('project-1'),
      ref<string | undefined>('run-1'),
      ref<string | undefined>('approval-1'),
    )

    await vi.waitFor(() => expect(approval.publication.kind).toBe('success'))
    expect(getProjectAuthoringApproval).toHaveBeenCalledWith({
      path: { projectId: 'project-1', approvalId: 'approval-1' },
    })
    expect(approval.approval.kind).toBe('success')
    expect(approval.publication.kind === 'success' && approval.publication.data.status).toBe('pending')
    expect(approval.canApprove).toBe(false)
    approval.stopPublicationPolling()
  })

  it('polls pending publication until the server reports ready', async () => {
    vi.useFakeTimers()
    try {
      vi.mocked(getProjectAuthoringApproval)
        .mockResolvedValueOnce({ data: makePublicationStatus('pending') as never, error: undefined as never })
        .mockResolvedValueOnce({ data: makePublicationStatus('ready') as never, error: undefined as never })
      const approval = useProjectAuthoringApproval(
        ref<string | null>('project-1'),
        ref<string | undefined>('run-1'),
        ref<string | undefined>('approval-1'),
      )

      await vi.waitFor(() => expect(approval.publication.kind).toBe('success'))
      expect(approval.publication.kind === 'success' && approval.publication.data.status).toBe('pending')
      await vi.advanceTimersByTimeAsync(3000)
      await vi.waitFor(() => expect(approval.publication.kind === 'success' && approval.publication.data.status).toBe('ready'))
      expect(getProjectAuthoringApproval).toHaveBeenCalledTimes(2)
    } finally {
      vi.useRealTimers()
    }
  })

  it('stops polling and exposes the server diagnostic when publication fails', async () => {
    vi.useFakeTimers()
    try {
      vi.mocked(getProjectAuthoringApproval)
        .mockResolvedValueOnce({ data: makePublicationStatus('publishing') as never, error: undefined as never })
        .mockResolvedValueOnce({ data: makePublicationStatus('failed') as never, error: undefined as never })
      const approval = useProjectAuthoringApproval(
        ref<string | null>('project-1'),
        ref<string | undefined>('run-1'),
        ref<string | undefined>('approval-1'),
      )

      await vi.waitFor(() => expect(approval.publication.kind).toBe('success'))
      await vi.advanceTimersByTimeAsync(3000)
      await vi.waitFor(() => expect(approval.publication.kind === 'success' && approval.publication.data.status).toBe('failed'))
      expect(approval.publication.kind === 'success' && approval.publication.data.diagnosticCode).toBe('AUTHORING_PUBLICATION_FAILED')
      await vi.advanceTimersByTimeAsync(6000)
      expect(getProjectAuthoringApproval).toHaveBeenCalledTimes(2)
    } finally {
      vi.useRealTimers()
    }
  })
})
