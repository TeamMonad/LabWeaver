import type { EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'
import { nowIso } from '../utils/clock'
import { nextUuid7 } from '../utils/identity'
import { nextRevision } from '../utils/sequence'

const releasesByCourse = new Map<string, EnvironmentTemplateReleaseViewSchema[]>()

function createPlaceholderArtifactRef(): EnvironmentTemplateReleaseViewSchema['artifact']['provenance'] {
  return {
    artifactId: nextUuid7('artifact'),
    mediaType: 'application/vnd.labweaver.fixture+tar',
    objectVersion: 'fixture-version',
    sha256: 'sha256:' + '0'.repeat(64),
    sizeBytes: 1024,
    storeBinding: 'fixture-store',
  }
}

function createPlaceholderSignature(): EnvironmentTemplateReleaseViewSchema['artifact']['signature'] {
  return {
    certificateSha256: 'sha256:' + '0'.repeat(64),
    certificateSubject: 'fixture@labweaver.io',
    ctLogId: 'fixture-ct-log',
    fulcioIssuer: 'https://fixture.fulcio.dev',
    rekorInclusionProofSha256: 'sha256:' + '0'.repeat(64),
    rekorLogId: 'fixture-rekor-log',
    rekorLogIndex: 1,
    sctSha256: 'sha256:' + '0'.repeat(64),
    signatureSha256: 'sha256:' + '0'.repeat(64),
    trustBundleSha256: 'sha256:' + '0'.repeat(64),
    verifiedAt: nowIso(),
  }
}

function createContainerArtifact(): EnvironmentTemplateReleaseViewSchema['artifact'] {
  return {
    kind: 'container',
    id: nextUuid7('image'),
    build_request_id: nextUuid7('build'),
    digest: 'sha256:' + 'c'.repeat(64),
    immutable_tag: 'fixture-container:v1',
    repository: 'registry.labweaver.local/fixture-container',
    provenance: createPlaceholderArtifactRef(),
    sbom: createPlaceholderArtifactRef(),
    signature: createPlaceholderSignature(),
  }
}

function createVirtualMachineArtifact(): EnvironmentTemplateReleaseViewSchema['artifact'] {
  return {
    kind: 'virtual_machine',
    id: nextUuid7('image'),
    format: 'qcow2',
    base_disk: createPlaceholderArtifactRef(),
    provenance: createPlaceholderArtifactRef(),
    sbom: createPlaceholderArtifactRef(),
    signature: createPlaceholderSignature(),
  }
}

function createRelease(
  courseId: string,
  runtimeKind: EnvironmentTemplateReleaseViewSchema['runtimeKind'],
): EnvironmentTemplateReleaseViewSchema {
  const releaseId = nextUuid7('release')
  const candidateId = nextUuid7('candidate')
  const approvalId = nextUuid7('approval')
  const policyId = nextUuid7('policy')
  const now = nowIso()
  return {
    id: releaseId,
    courseId,
    candidateId,
    candidateRevision: nextRevision(),
    version: nextRevision(),
    runtimeKind,
    environmentSpecSha256: 'sha256:' + 'e'.repeat(64),
    publishedAt: now,
    publishedBy: 'fixture-actor-teacher',
    artifact: runtimeKind === 'container' ? createContainerArtifact() : createVirtualMachineArtifact(),
    approval: {
      id: approvalId,
      candidateId,
      candidateRevision: nextRevision(),
      candidateSha256: 'sha256:' + 'a'.repeat(64),
      decidedAt: now,
      decision: 'approved',
      actorId: 'fixture-actor-teacher',
      reason: 'fixture approval',
      policyRevision: nextRevision(),
      schemaSha256: 'sha256:' + 's'.repeat(64),
      trustRevision: nextRevision(),
    },
    imagePolicyEvaluation: {
      artifactId: nextUuid7('image'),
      artifactSha256: 'sha256:' + 'i'.repeat(64),
      evaluatedAt: now,
      expectedCertificateSubject: 'fixture@labweaver.io',
      expectedFulcioIssuer: 'https://fixture.fulcio.dev',
      maxEvidenceAgeMilliseconds: 3600000,
      passed: true,
      policyId,
      policyRevision: nextRevision(),
      requireCtSct: true,
      requireRekorInclusion: true,
      scannerDatabaseSha256: 'sha256:' + 'd'.repeat(64),
      scannerName: 'fixture-scanner',
      scannerVersion: '1.0.0',
      trustBundleSha256: 'sha256:' + 't'.repeat(64),
      validUntil: nowIso(),
      vulnerabilities: { critical: 0, high: 0, medium: 0, low: 0, unknown: 0 },
    },
    withdrawal: null,
  }
}

export function seedTemplateReleases(courseIds: string[]): void {
  releasesByCourse.clear()
  for (const courseId of courseIds) {
    releasesByCourse.set(courseId, [
      createRelease(courseId, 'container'),
      createRelease(courseId, 'virtual_machine'),
    ])
  }
}

export function listTemplateReleases(courseId: string): EnvironmentTemplateReleaseViewSchema[] {
  return releasesByCourse.get(courseId) ?? []
}

export function resetTemplateReleaseStore(): void {
  releasesByCourse.clear()
}
