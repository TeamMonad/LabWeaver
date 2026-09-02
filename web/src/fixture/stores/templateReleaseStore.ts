import type { EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'
import { addHoursIso, nowIso } from '../utils/clock'
import { nextUuid7 } from '../utils/identity'
import { nextRevision } from '../utils/sequence'

const releasesByCourse = new Map<string, EnvironmentTemplateReleaseViewSchema[]>()

function createPlaceholderArtifactRef(): Extract<EnvironmentTemplateReleaseViewSchema['artifact'], { kind: 'virtual_machine' }>['base_disk'] {
  return {
    binding: 'linux-lab-base-v1',
    sourceRegistryDigest: `docker://registry.labweaver.local/vm/linux-lab@sha256:${'0'.repeat(64)}`,
    diskSha256: '1'.repeat(64),
    capacityBytes: 1073741824,
  }
}

function createContainerArtifact(): EnvironmentTemplateReleaseViewSchema['artifact'] {
  return {
    kind: 'container',
    id: nextUuid7('image'),
    build_request_id: nextUuid7('build'),
    digest: 'sha256:' + 'c'.repeat(64),
    repository: 'registry.labweaver.local/fixture-container',
  }
}

function createVirtualMachineArtifact(): EnvironmentTemplateReleaseViewSchema['artifact'] {
  return {
    kind: 'virtual_machine',
    id: nextUuid7('image'),
    format: 'qcow2',
    base_disk: createPlaceholderArtifactRef(),
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
  const artifact = runtimeKind === 'container' ? createContainerArtifact() : createVirtualMachineArtifact()
  return {
    id: releaseId,
    courseId,
    candidateId,
    candidateRevision: nextRevision(),
    version: nextRevision(),
    runtimeKind,
    environmentSpecSha256: 'e'.repeat(64),
    publishedAt: now,
    publishedBy: 'fixture-actor-teacher',
    artifact,
    approval: {
      id: approvalId,
      candidateId,
      candidateRevision: nextRevision(),
      candidateSha256: 'a'.repeat(64),
      decidedAt: now,
      decision: 'approved',
      actorId: 'fixture-actor-teacher',
      reason: 'fixture approval',
      policyRevision: nextRevision(),
      schemaSha256: 's'.repeat(64),
      trustRevision: nextRevision(),
    },
    imagePolicyEvaluation: artifact.kind === 'container' ? {
      artifactId: artifact.id,
      artifactSha256: artifact.digest.replace(/^sha256:/, ''),
      evaluatedAt: now,
      maxEvidenceAgeMilliseconds: 3600000,
      passed: true,
      policyId,
      policyRevision: nextRevision(),
      scannerDatabaseSha256: 'd'.repeat(64),
      scannerName: 'fixture-scanner',
      scannerVersion: '1.0.0',
      validUntil: addHoursIso(1),
      vulnerabilities: { critical: 0, high: 0, medium: 0, low: 0, unknown: 0 },
    } : null,
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

export function publishRelease(
  release: EnvironmentTemplateReleaseViewSchema,
): EnvironmentTemplateReleaseViewSchema {
  const existing = releasesByCourse.get(release.courseId) ?? []
  releasesByCourse.set(release.courseId, [...existing, release])
  return release
}

export function resetTemplateReleaseStore(): void {
  releasesByCourse.clear()
}
