import type {
  EnvironmentCandidateSchema,
  EvaluationCandidateSchema,
  EnvironmentSpec,
  EvaluationSpec,
  ImageArtifact,
  ImagePolicyEvaluation,
  RuntimeKind,
  SigstoreEvidence,
  VulnerabilitySummary,
} from '@/generated/contracts'
import { nowIso } from '../utils/clock'
import { nextUuid7 } from '../utils/identity'
import { nextRevision } from '../utils/sequence'

export interface StoredEnvironmentCandidate {
  candidate: EnvironmentCandidateSchema
  imageArtifact: ImageArtifact
  imagePolicyEvaluation: ImagePolicyEvaluation
  trustRevision: number
}

export interface StoredEvaluationCandidate {
  candidate: EvaluationCandidateSchema
  trustRevision: number
}

const environmentCandidates = new Map<string, StoredEnvironmentCandidate>()
const evaluationCandidates = new Map<string, StoredEvaluationCandidate>()

function sha256(prefix: string): string {
  return `sha256:${prefix.repeat(64)}`
}

function artifactRef(mediaType: string, prefix: string) {
  return {
    artifactId: nextUuid7('artifact'),
    mediaType,
    objectVersion: 'version-1',
    sha256: sha256(prefix),
    sizeBytes: 1024,
    storeBinding: 'fixture-store',
  }
}

function createEnvironmentSpec(runtimeKind: RuntimeKind, courseId: string): EnvironmentSpec {
  const base = {
    apiVersion: 'environment.labweaver.io/v1' as const,
    kind: 'EnvironmentSpec' as const,
    name: `labweaver-${courseId}-${runtimeKind}`,
    class: 'experiment' as const,
    entries: [{ name: 'code-server', protocol: 'https' as const, servicePort: 8080 }],
    network: { mode: 'deny_all' as const },
    resources: { cpuMillicores: 1000, memoryBytes: 1073741824, storageBytes: 1073741824 },
    retention: {
      class: 'build_evidence' as const,
      disposition: 'retain_sanitized_receipt' as const,
      policyId: 'policy-1',
      policyRevision: 1,
      retainUntil: '2026-08-01T00:00:00.000Z',
    },
    security: {
      userPolicy: 'non_root_required' as const,
      rootFilesystemPolicy: 'read_only_required' as const,
      privilegeEscalationPolicy: 'deny' as const,
      publicExposurePolicy: 'deny' as const,
      securityProfileBinding: 'restricted-v1',
    },
  }

  if (runtimeKind === 'container') {
    return {
      ...base,
      runtime: {
        kind: 'container',
        provider_binding: 'container-primary-v1',
        build_context: artifactRef('application/vnd.oci.image.layer.v1.tar+gzip', 'b'),
        base_image_digest: sha256('c'),
        service_port: 8080,
      },
    }
  }

  return {
    ...base,
    runtime: {
      kind: 'virtual_machine',
      provider_binding: 'kubevirt-primary-v1',
      base_disk: artifactRef('application/vnd.qemu.qcow2', 'd'),
      ssh_port: 22,
      storage_class_binding: 'storage-primary-v1',
    },
  }
}

function createEvaluationSpec(courseId: string): EvaluationSpec {
  return {
    apiVersion: 'evaluation.labweaver.io/v1',
    kind: 'EvaluationSpec',
    metadata: { name: `evaluation-${courseId}`, version: '1.0.0' },
    spec: {
      submission: {
        include: ['workspace/**/*'],
        maxBytes: 10485760,
        kind: 'workspace_snapshot',
      },
      steps: [
        {
          name: 'check',
          runner: {
            kind: 'file_assertion',
            requiredFiles: ['README.md', 'main.py'],
          },
        },
      ],
      aggregation: {
        kind: 'deterministic_sum',
        maxScore: 100,
        gates: [{ step: 'check', requiredStatus: 'passed' }],
      },
      review: {
        include: ['README.md'],
        kind: 'llm_review',
        outputMode: 'goal_assessment',
        rubric: 'rubric-v1',
        failurePolicy: 'continue_advisory',
      },
    },
  }
}

function createSigstoreEvidence(prefix: string): SigstoreEvidence {
  return {
    certificateSha256: sha256(`${prefix}cert`),
    certificateSubject: 'spiffe://labweaver/image-builder',
    ctLogId: 'fixture-ct-log',
    fulcioIssuer: 'https://fixture.fulcio.dev',
    rekorInclusionProofSha256: sha256(`${prefix}proof`),
    rekorLogId: 'fixture-rekor-log',
    rekorLogIndex: 1,
    sctSha256: sha256(`${prefix}sct`),
    signatureSha256: sha256(`${prefix}sig`),
    subjectDigest: sha256('e'),
    trustBundleSha256: sha256('trust'),
    verifiedAt: nowIso(),
  }
}

function createImageArtifact(runtimeKind: RuntimeKind, candidateId: string): ImageArtifact {
  const artifactId = nextUuid7('image')
  const buildRequestId = nextUuid7('build')
  const digest = sha256('e')
  const signature = createSigstoreEvidence('i')

  if (runtimeKind === 'container') {
    return {
      kind: 'container',
      id: artifactId,
      build_request_id: buildRequestId,
      repository: `registry.labweaver.local/${candidateId}`,
      immutable_tag: `release-${nextRevision()}`,
      digest,
      provenance: artifactRef('application/vnd.in-toto+json', 'p'),
      sbom: artifactRef('application/spdx+json', 's'),
      signature,
    }
  }

  return {
    kind: 'virtual_machine',
    id: artifactId,
    format: 'qcow2',
    base_disk: artifactRef('application/vnd.qemu.qcow2', 'd'),
    provenance: artifactRef('application/vnd.in-toto+json', 'p'),
    sbom: artifactRef('application/spdx+json', 's'),
    signature,
  }
}

function createImagePolicyEvaluation(artifact: ImageArtifact): ImagePolicyEvaluation {
  const artifactId = artifact.id
  const artifactSha256 = artifact.kind === 'container' ? artifact.digest : sha256('e')
  const vulnerabilities: VulnerabilitySummary = { critical: 0, high: 0, medium: 0, low: 0, unknown: 0 }
  const now = nowIso()
  return {
    artifactId,
    artifactSha256,
    evaluatedAt: now,
    expectedCertificateSubject: 'spiffe://labweaver/image-builder',
    expectedFulcioIssuer: 'https://fixture.fulcio.dev',
    maxEvidenceAgeMilliseconds: 3600000,
    passed: true,
    policyId: 'image-policy-1',
    policyRevision: 1,
    requireCtSct: true,
    requireRekorInclusion: true,
    scannerDatabaseSha256: sha256('scanner-db'),
    scannerName: 'trivy',
    scannerVersion: '1.0.0',
    trustBundleSha256: sha256('trust'),
    validUntil: now,
    vulnerabilities,
  }
}

export function createEnvironmentCandidate(
  runId: string,
  courseId: string,
  runtimeKind: RuntimeKind,
  policyRevision: number,
  schemaSha256: string,
  candidateId?: string,
): StoredEnvironmentCandidate {
  const id = candidateId ?? nextUuid7('candidate')
  const spec = createEnvironmentSpec(runtimeKind, courseId)
  const candidate: EnvironmentCandidateSchema = {
    createdAt: nowIso(),
    id,
    model: 'claude-sonnet-4-5',
    policyRevision,
    revision: nextRevision(),
    runId,
    schemaSha256,
    spec,
    specSha256: sha256('e'),
  }
  const imageArtifact = createImageArtifact(runtimeKind, id)
  const imagePolicyEvaluation = createImagePolicyEvaluation(imageArtifact)
  const stored: StoredEnvironmentCandidate = {
    candidate,
    imageArtifact,
    imagePolicyEvaluation,
    trustRevision: 1,
  }
  environmentCandidates.set(id, stored)
  return stored
}

export function createEvaluationCandidate(
  runId: string,
  courseId: string,
  policyRevision: number,
  schemaSha256: string,
  candidateId?: string,
): StoredEvaluationCandidate {
  const id = candidateId ?? nextUuid7('candidate')
  const candidate: EvaluationCandidateSchema = {
    createdAt: nowIso(),
    id,
    model: 'claude-sonnet-4-5',
    policyRevision,
    revision: nextRevision(),
    runId,
    schemaSha256,
    spec: createEvaluationSpec(courseId),
    specSha256: sha256('e'),
  }
  const stored: StoredEvaluationCandidate = { candidate, trustRevision: 1 }
  evaluationCandidates.set(id, stored)
  return stored
}

export function getEnvironmentCandidate(candidateId: string): StoredEnvironmentCandidate | undefined {
  return environmentCandidates.get(candidateId)
}

export function getEvaluationCandidate(candidateId: string): StoredEvaluationCandidate | undefined {
  return evaluationCandidates.get(candidateId)
}

export function resetCandidateStore(): void {
  environmentCandidates.clear()
  evaluationCandidates.clear()
}
