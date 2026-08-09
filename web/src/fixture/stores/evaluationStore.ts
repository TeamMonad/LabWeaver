import type {
  CandidateApprovalSchema,
  CreateEvaluationReleaseRequestSchema,
  EvaluationReleaseSchema,
  StudentEvaluationResultSchema,
} from '@/generated/contracts'
import { addSecondsIso, nowIso } from '../utils/clock'
import { nextUuid7 } from '../utils/identity'
import { getEvaluationCandidate, createEvaluationCandidate } from './candidateStore'

interface StoredResult {
  actorId: string
  result: StudentEvaluationResultSchema
}

type MutationResult =
  | { kind: 'ok'; release: EvaluationReleaseSchema }
  | { kind: 'conflict' }
  | { kind: 'not-found' }
  | { kind: 'precondition' }

const releases = new Map<string, EvaluationReleaseSchema>()
const results = new Map<string, StoredResult>()
const publicationIdempotency = new Map<string, { fingerprint: string; releaseId: string }>()
const withdrawalIdempotency = new Map<string, { fingerprint: string; releaseId: string }>()

const hash = (value: string) => value.repeat(64)

function runtimeIdentity() {
  return {
    configurationSha256: hash('a'),
    migrationCatalogSha256: hash('b'),
    packageSha256: hash('c'),
    providerBinding: 'kubernetes/evaluation-primary-v1',
    runnerImage: `registry.labweaver.local/evaluation-worker@sha256:${hash('d')}`,
    runtimeArtifactSha256: hash('f'),
    sourceSha256: hash('e'),
  }
}

function publicationFingerprint(
  courseId: string,
  request: CreateEvaluationReleaseRequestSchema,
): string {
  return JSON.stringify({ courseId, ...request })
}

export function createEvaluationRelease(
  courseId: string,
  request: CreateEvaluationReleaseRequestSchema,
  approval: CandidateApprovalSchema,
  actorId: string,
  idempotencyKey: string,
): MutationResult {
  const fingerprint = publicationFingerprint(courseId, request)
  const replay = publicationIdempotency.get(idempotencyKey)
  if (replay) {
    if (replay.fingerprint !== fingerprint) return { kind: 'conflict' }
    const release = releases.get(replay.releaseId)
    return release ? { kind: 'ok', release: structuredClone(release) } : { kind: 'not-found' }
  }
  const stored = getEvaluationCandidate(request.candidateId)
  if (
    !stored ||
    stored.candidate.revision !== request.candidateRevision ||
    stored.candidate.specSha256 !== request.evaluationSpecSha256 ||
    approval.candidateId !== request.candidateId ||
    approval.candidateRevision !== request.candidateRevision ||
    approval.candidateSha256 !== request.evaluationSpecSha256 ||
    approval.decision !== 'approved'
  ) {
    return { kind: 'precondition' }
  }
  const release: EvaluationReleaseSchema = {
    approvalId: approval.id,
    approvalRevision: 1,
    approvalSha256: hash('d'),
    candidateId: stored.candidate.id,
    candidateRevision: stored.candidate.revision,
    candidateSha256: stored.candidate.specSha256,
    courseId,
    evaluationSpec: structuredClone(stored.candidate.spec),
    evaluationSpecSha256: stored.candidate.specSha256,
    id: nextUuid7(),
    publishedAt: nowIso(),
    publishedBy: actorId,
    revision: 1,
    runtimeIdentity: runtimeIdentity(),
    schemaVersion: 'evaluation.labweaver.io/evaluation-release/v1',
    state: 'active',
  }
  releases.set(release.id, release)
  publicationIdempotency.set(idempotencyKey, { fingerprint, releaseId: release.id })
  return { kind: 'ok', release: structuredClone(release) }
}

export function listEvaluationReleases(
  courseId: string,
  cursor?: string,
  limit = 50,
): { items: EvaluationReleaseSchema[]; nextCursor?: string | null } | 'invalid-cursor' {
  const ordered = Array.from(releases.values())
    .filter((release) => release.courseId === courseId)
    .sort((left, right) => right.publishedAt.localeCompare(left.publishedAt) || right.id.localeCompare(left.id))
  const start = cursor ? ordered.findIndex((release) => release.id === cursor) + 1 : 0
  if (cursor && start === 0) return 'invalid-cursor'
  const page = ordered.slice(start, start + limit)
  return {
    items: structuredClone(page),
    nextCursor: ordered.length > start + limit ? page.at(-1)?.id ?? null : null,
  }
}

export function getEvaluationRelease(courseId: string, releaseId: string): EvaluationReleaseSchema | undefined {
  const release = releases.get(releaseId)
  return release?.courseId === courseId ? structuredClone(release) : undefined
}

export function withdrawEvaluationRelease(
  courseId: string,
  releaseId: string,
  expectedRevision: number,
  reasonCode: string,
  idempotencyKey: string,
): MutationResult {
  const fingerprint = JSON.stringify({ courseId, releaseId, expectedRevision, reasonCode })
  const replay = withdrawalIdempotency.get(idempotencyKey)
  if (replay) {
    if (replay.fingerprint !== fingerprint) return { kind: 'conflict' }
    const release = releases.get(replay.releaseId)
    return release ? { kind: 'ok', release: structuredClone(release) } : { kind: 'not-found' }
  }
  const release = releases.get(releaseId)
  if (!release || release.courseId !== courseId) return { kind: 'not-found' }
  if (release.revision !== expectedRevision || release.state !== 'active') {
    return { kind: 'precondition' }
  }
  release.state = 'withdrawn'
  release.revision += 1
  release.withdrawnAt = nowIso()
  release.withdrawalDiagnosticCode = reasonCode
  withdrawalIdempotency.set(idempotencyKey, { fingerprint, releaseId })
  return { kind: 'ok', release: structuredClone(release) }
}

export function listStudentResults(
  courseId: string,
  actorId: string,
  cursor?: string,
  limit = 50,
): { items: StudentEvaluationResultSchema[]; nextCursor?: string | null } | 'invalid-cursor' {
  const ordered = Array.from(results.values())
    .filter((entry) => entry.actorId === actorId && entry.result.courseId === courseId)
    .map((entry) => entry.result)
    .sort((left, right) => right.updatedAt.localeCompare(left.updatedAt) || right.runId.localeCompare(left.runId))
  const start = cursor ? ordered.findIndex((result) => result.runId === cursor) + 1 : 0
  if (cursor && start === 0) return 'invalid-cursor'
  const page = ordered.slice(start, start + limit)
  return {
    items: structuredClone(page),
    nextCursor: ordered.length > start + limit ? page.at(-1)?.runId ?? null : null,
  }
}

export function getStudentResult(
  courseId: string,
  actorId: string,
  runId: string,
): StudentEvaluationResultSchema | undefined {
  const entry = results.get(runId)
  return entry?.actorId === actorId && entry.result.courseId === courseId
    ? structuredClone(entry.result)
    : undefined
}

function seedResult(
  releaseId: string,
  state: 'succeeded' | 'failed' | 'cancelled',
  offsetSeconds: number,
): void {
  const succeeded = state === 'succeeded'
  const diagnosticCode = state === 'failed'
    ? 'LW_EVALUATION_CHECK_FAILED'
    : state === 'cancelled'
      ? 'LW_EVALUATION_CANCELLED'
      : undefined
  const runId = nextUuid7()
  results.set(runId, {
    actorId: 'fixture-actor-student',
    result: {
      ...(succeeded ? { awardedScore: 92 } : {}),
      completedAt: addSecondsIso(offsetSeconds),
      courseId: 'course-101',
      createdAt: addSecondsIso(offsetSeconds - 60),
      ...(diagnosticCode ? { diagnosticCode } : {}),
      frozenSubmissionId: nextUuid7(),
      maxScore: 100,
      releaseId,
      revision: 4,
      runId,
      state,
      steps: [
        {
          ...(succeeded ? { awardedScore: 92 } : {}),
          ...(diagnosticCode ? { diagnosticCode } : {}),
          maxScore: 100,
          position: 0,
          role: 'score',
          state: succeeded ? 'succeeded' : state,
        },
      ],
      updatedAt: addSecondsIso(offsetSeconds),
    },
  })
}

export function seedEvaluationData(): void {
  const candidate = createEvaluationCandidate(
    'fixture-seed-evaluation-run',
    'course-101',
    1,
    hash('a'),
  ).candidate
  const release: EvaluationReleaseSchema = {
    approvalId: nextUuid7(),
    approvalRevision: 1,
    approvalSha256: hash('d'),
    candidateId: candidate.id,
    candidateRevision: candidate.revision,
    candidateSha256: candidate.specSha256,
    courseId: 'course-101',
    evaluationSpec: candidate.spec,
    evaluationSpecSha256: candidate.specSha256,
    id: nextUuid7(),
    publishedAt: addSecondsIso(-600),
    publishedBy: 'fixture-actor-teacher',
    revision: 1,
    runtimeIdentity: runtimeIdentity(),
    schemaVersion: 'evaluation.labweaver.io/evaluation-release/v1',
    state: 'active',
  }
  releases.set(release.id, release)
  seedResult(release.id, 'succeeded', -300)
  seedResult(release.id, 'failed', -200)
  seedResult(release.id, 'cancelled', -100)
}

export function resetEvaluationStore(): void {
  releases.clear()
  results.clear()
  publicationIdempotency.clear()
  withdrawalIdempotency.clear()
}
