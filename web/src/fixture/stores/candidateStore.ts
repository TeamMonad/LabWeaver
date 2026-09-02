import type {
  EnvironmentCandidateSchema,
  EvaluationCandidateSchema,
  EnvironmentSpec,
  EvaluationSpec,
  ImageArtifact,
  ImagePolicyEvaluation,
  RuntimeKind,
  VulnerabilitySummary,
} from '@/generated/contracts'
import { addHoursIso, nowIso } from '../utils/clock'
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

function sha256Hex(prefix: string): string {
  return prefix.repeat(64)
}

function ociDigest(prefix: string): string {
  return `sha256:${sha256Hex(prefix)}`
}

function artifactRef(mediaType: string, prefix: string) {
  return {
    artifactId: nextUuid7('artifact'),
    mediaType,
    objectVersion: 'version-1',
    sha256: sha256Hex(prefix),
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
        base_image_digest: ociDigest('c'),
        service_port: 8080,
      },
    }
  }

  return {
    ...base,
    runtime: {
      kind: 'virtual_machine',
      provider_binding: 'kubevirt-primary-v1',
      base_disk: {
        binding: 'linux-lab-base-v1',
        sourceRegistryDigest: `docker://registry.labweaver.local/vm/linux-lab@${ociDigest('d')}`,
        diskSha256: sha256Hex('d'),
        capacityBytes: 1073741824,
      },
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

function createImageArtifact(runtimeKind: RuntimeKind, candidateId: string): ImageArtifact {
  const artifactId = nextUuid7('image')
  const buildRequestId = nextUuid7('build')
  const digest = ociDigest('e')

  if (runtimeKind === 'container') {
    return {
      kind: 'container',
      id: artifactId,
      build_request_id: buildRequestId,
      repository: `registry.labweaver.local/${candidateId}`,
      digest,
    }
  }

  return {
    kind: 'virtual_machine',
    id: artifactId,
    format: 'qcow2',
    base_disk: {
      binding: 'linux-lab-base-v1',
      sourceRegistryDigest: `docker://registry.labweaver.local/vm/linux-lab@${ociDigest('d')}`,
      diskSha256: sha256Hex('d'),
      capacityBytes: 1073741824,
    },
  }
}

function createImagePolicyEvaluation(artifact: ImageArtifact): ImagePolicyEvaluation {
  const artifactId = artifact.id
  const artifactSha256 = artifact.kind === 'container'
    ? artifact.digest.replace(/^sha256:/, '')
    : artifact.base_disk.diskSha256
  const vulnerabilities: VulnerabilitySummary = { critical: 0, high: 0, medium: 0, low: 0, unknown: 0 }
  const now = nowIso()
  return {
    artifactId,
    artifactSha256,
    evaluatedAt: now,
    maxEvidenceAgeMilliseconds: 3600000,
    passed: true,
    policyId: 'image-policy-1',
    policyRevision: 1,
    scannerDatabaseSha256: sha256Hex('f'),
    scannerName: 'trivy',
    scannerVersion: '1.0.0',
    validUntil: addHoursIso(1),
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
    specSha256: sha256Hex('e'),
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
    specSha256: sha256Hex('e'),
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
