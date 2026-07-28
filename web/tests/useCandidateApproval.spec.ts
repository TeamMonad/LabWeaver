import { beforeEach, describe, expect, it, vi } from 'vitest'
import { ref } from 'vue'
import { useCandidateApproval } from '@/composables/useCandidateApproval'
import {
  appendEnvironmentCandidateDecision,
  createEnvironmentTemplateRelease,
  getAgentRun,
  getEnvironmentCandidate,
  getEvaluationCandidate,
  listEnvironmentTemplateReleases,
} from '@/generated/contracts'
import type { EnvironmentCandidateViewSchema } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    appendEnvironmentCandidateDecision: vi.fn(),
    appendEvaluationCandidateDecision: vi.fn(),
    createEnvironmentTemplateRelease: vi.fn(),
    getAgentRun: vi.fn(),
    getEnvironmentCandidate: vi.fn(),
    getEvaluationCandidate: vi.fn(),
    listEnvironmentTemplateReleases: vi.fn(),
  }
})

function makeRun() {
  return {
    id: 'run-1',
    courseId: 'course-1',
    packageId: 'pkg-1',
    policyId: 'policy-1',
    policyRevision: 1,
    requestedRuntime: 'container' as const,
    revision: 1,
    state: 'succeeded' as const,
    tracks: [
      { kind: 'environment' as const, candidateId: 'env-cand-1', attempts: [] },
      { kind: 'evaluation' as const, candidateId: 'eval-cand-1', attempts: [] },
    ],
  }
}

function makeEnvCandidate() {
  return {
    candidate: {
      id: 'env-cand-1',
      runId: 'run-1',
      model: 'claude-sonnet-4-5',
      policyRevision: 1,
      revision: 2,
      schemaSha256: 'sha256:schema',
      spec: {
        name: 'env',
        runtime: { kind: 'container', provider_binding: 'x', build_context: {}, base_image_digest: 'x', service_port: 8080 },
      },
      specSha256: 'sha256:spec',
      createdAt: '2026-07-16T08:00:00.000Z',
    },
    approvals: [],
    build: {
      state: 'succeeded' as const,
      artifact: {
        kind: 'container' as const,
        id: 'image-1',
        build_request_id: 'build-1',
        repository: 'registry.labweaver.local/candidate-1',
        digest: 'sha256:image',
      },
      imagePolicyEvaluation: {
        artifactId: 'image-1',
        artifactSha256: 'image',
        evaluatedAt: '2026-07-16T08:00:00.000Z',
        maxEvidenceAgeMilliseconds: 3600000,
        passed: true,
        policyId: 'policy-1',
        policyRevision: 1,
        scannerDatabaseSha256: 'sha256:scanner-db',
        scannerName: 'trivy',
        scannerVersion: '1.0.0',
        validUntil: '2026-07-16T10:00:00.000Z',
        vulnerabilities: { critical: 0, high: 0, medium: 0, low: 0, unknown: 0 },
      },
    },
    trustRevision: 1,
  }
}

function makeEvalCandidate() {
  return {
    candidate: {
      id: 'eval-cand-1',
      runId: 'run-1',
      model: 'claude-sonnet-4-5',
      policyRevision: 1,
      revision: 1,
      schemaSha256: 'sha256:schema',
      spec: { name: 'eval' },
      specSha256: 'sha256:spec',
      createdAt: '2026-07-16T08:00:00.000Z',
    },
    approvals: [],
    trustRevision: 1,
  }
}

function makeVmCandidate(): EnvironmentCandidateViewSchema {
  const view = structuredClone(makeEnvCandidate()) as EnvironmentCandidateViewSchema
  view.candidate.spec.runtime = {
    kind: 'virtual_machine',
    provider_binding: 'kubevirt-primary-v1',
    storage_class_binding: 'vm-rwo-primary-v1',
    ssh_port: 22,
    base_disk: {
      binding: 'ubuntu-24.04-v1',
      source_registry_digest: `docker://registry.invalid/ubuntu@sha256:${'a'.repeat(64)}`,
      disk_sha256: 'b'.repeat(64),
      capacity_bytes: 10_737_418_240,
    },
  }
  delete view.build
  return view
}

describe('useCandidateApproval', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    vi.mocked(getAgentRun).mockResolvedValue({ data: makeRun(), error: undefined as never })
    vi.mocked(getEnvironmentCandidate).mockResolvedValue({ data: makeEnvCandidate(), error: undefined as never })
    vi.mocked(getEvaluationCandidate).mockResolvedValue({ data: makeEvalCandidate(), error: undefined as never })
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined as never })
  })

  it('loads run and both candidates', async () => {
    const courseId = ref('course-1')
    const runId = ref('run-1')
    const approval = useCandidateApproval(courseId, runId)

    await vi.waitFor(() => expect(approval.run.kind).toBe('success'))
    expect(approval.environmentCandidate.kind).toBe('success')
    expect(approval.evaluationCandidate.kind).toBe('success')
  })

  it('approves environment candidate and enables publish', async () => {
    const courseId = ref('course-1')
    const runId = ref('run-1')
    const approval = useCandidateApproval(courseId, runId)
    await vi.waitFor(() => expect(approval.environmentCandidate.kind).toBe('success'))

    vi.mocked(appendEnvironmentCandidateDecision).mockResolvedValue({
      data: {
        id: 'approval-1',
        actorId: 'teacher',
        candidateId: 'env-cand-1',
        candidateRevision: 2,
        candidateSha256: 'sha256:spec',
        decidedAt: '2026-07-16T09:00:00.000Z',
        decision: 'approved',
        policyRevision: 1,
        reason: 'ok',
        schemaSha256: 'sha256:schema',
        trustRevision: 1,
      },
      error: undefined as never,
    })

    await approval.decide('environment', 'approved', 'ok')
    expect(approval.latestEnvironmentApproval?.decision).toBe('approved')
    expect(approval.canPublish).toBe(true)
  })

  it('publishes release with candidate identity while Control resolves evidence', async () => {
    const courseId = ref('course-1')
    const runId = ref('run-1')
    const approval = useCandidateApproval(courseId, runId)
    await vi.waitFor(() => expect(approval.environmentCandidate.kind).toBe('success'))

    vi.mocked(appendEnvironmentCandidateDecision).mockResolvedValue({
      data: {
        id: 'approval-1',
        actorId: 'teacher',
        candidateId: 'env-cand-1',
        candidateRevision: 2,
        candidateSha256: 'sha256:spec',
        decidedAt: '2026-07-16T09:00:00.000Z',
        decision: 'approved',
        policyRevision: 1,
        reason: 'ok',
        schemaSha256: 'sha256:schema',
        trustRevision: 1,
      },
      error: undefined as never,
    })
    await approval.decide('environment', 'approved', 'ok')

    vi.mocked(createEnvironmentTemplateRelease).mockResolvedValue({
      data: { operationId: 'op-1', revision: 1, statusUrl: '/x' },
      error: undefined as never,
    })
    await approval.publishRelease()
    expect(createEnvironmentTemplateRelease).toHaveBeenCalledWith(
      expect.objectContaining({
        body: expect.objectContaining({
          approvalId: 'approval-1',
          candidateId: 'env-cand-1',
          candidateRevision: 2,
          environmentSpecSha256: 'sha256:spec',
          runtimeKind: 'container',
        }),
      }),
    )
    expect(approval.publish.kind).toBe('success')
  })

  it('allows an approved VM release without fabricated Container scan evidence', async () => {
    vi.mocked(getEnvironmentCandidate).mockResolvedValue({ data: makeVmCandidate(), error: undefined as never })
    const approval = useCandidateApproval(ref('course-1'), ref('run-1'))
    await vi.waitFor(() => expect(approval.environmentCandidate.kind).toBe('success'))

    vi.mocked(appendEnvironmentCandidateDecision).mockResolvedValue({
      data: {
        id: 'approval-vm-1',
        actorId: 'teacher',
        candidateId: 'env-cand-1',
        candidateRevision: 2,
        candidateSha256: 'sha256:spec',
        decidedAt: '2026-07-16T09:00:00.000Z',
        decision: 'approved',
        policyRevision: 1,
        reason: 'approved VM base',
        schemaSha256: 'sha256:schema',
        trustRevision: 1,
      },
      error: undefined as never,
    })
    await approval.decide('environment', 'approved', 'approved VM base')
    expect(approval.imageGate.status).toBe('blocked')
    expect(approval.canPublish).toBe(true)

    vi.mocked(createEnvironmentTemplateRelease).mockResolvedValue({
      data: { operationId: 'op-vm-1', revision: 1, statusUrl: '/x' },
      error: undefined as never,
    })
    await approval.publishRelease()
    expect(createEnvironmentTemplateRelease).toHaveBeenCalledWith(
      expect.objectContaining({
        body: expect.objectContaining({ runtimeKind: 'virtual_machine' }),
      }),
    )
  })

  it('does not publish when image gate is blocked', async () => {
    const courseId = ref('course-1')
    const runId = ref('run-1')
    const approval = useCandidateApproval(courseId, runId)
    await vi.waitFor(() => expect(approval.environmentCandidate.kind).toBe('success'))

    const blocked = makeEnvCandidate()
    blocked.build.imagePolicyEvaluation.vulnerabilities.critical = 1
    vi.mocked(getEnvironmentCandidate).mockResolvedValue({ data: blocked, error: undefined as never })
    await approval.load()
    await vi.waitFor(() => expect(approval.environmentCandidate.kind).toBe('success'))

    expect(approval.imageGate.status).toBe('blocked')
    expect(approval.canPublish).toBe(false)
  })

  it('surfaces decision conflict as error diagnostic', async () => {
    const courseId = ref('course-1')
    const runId = ref('run-1')
    const approval = useCandidateApproval(courseId, runId)
    await vi.waitFor(() => expect(approval.environmentCandidate.kind).toBe('success'))

    vi.mocked(appendEnvironmentCandidateDecision).mockResolvedValue({
      data: undefined as never,
      error: {
        response: {
          data: {
            diagnosticCode: 'REVISION_CONFLICT',
            detail: 'stale revision',
            retryable: false,
          },
        },
      },
    })

    await approval.decide('environment', 'approved', 'ok')
    expect(approval.environmentCandidate.kind).toBe('error')
    if (approval.environmentCandidate.kind === 'error') {
      expect(approval.environmentCandidate.diagnostic.code).toBe('REVISION_CONFLICT')
    }
  })
})
