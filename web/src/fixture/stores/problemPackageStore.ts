import type {
  CreateProblemPackageUploadRequestSchema,
  ProblemPackageSchema,
  ProblemPackageUploadSessionSchema,
} from '@/generated/contracts'
import { addHoursIso, addSecondsIso, nowIso } from '../utils/clock'
import { nextUuid7 } from '../utils/identity'
import { nextRevision } from '../utils/sequence'
import { getActivePolicy } from './llmPolicyStore'

const SHA256_PATTERN = /^[0-9a-f]{64}$/

const sessions = new Map<string, ProblemPackageUploadSessionSchema>()
const packages = new Map<string, ProblemPackageSchema>()
const sessionIdempotency = new Map<string, ProblemPackageUploadSessionSchema>()
const completeIdempotency = new Map<string, ProblemPackageSchema>()

export type CreateSessionResult =
  | { kind: 'ok'; session: ProblemPackageUploadSessionSchema }
  | { kind: 'invalid'; detail: string }
  | { kind: 'policy-revision-mismatch' }

export type CompleteSessionResult =
  | { kind: 'ok'; package: ProblemPackageSchema }
  | { kind: 'not-found' }
  | { kind: 'revision-mismatch' }
  | { kind: 'invalid'; detail: string }

function isValidSha256(value: string): boolean {
  return SHA256_PATTERN.test(value)
}

export function createUploadSession(
  courseId: string,
  request: CreateProblemPackageUploadRequestSchema,
  idempotencyKey: string,
): CreateSessionResult {
  const cached = sessionIdempotency.get(idempotencyKey)
  if (cached) return { kind: 'ok', session: cached }

  if (!Array.isArray(request.files) || request.files.length === 0) {
    return { kind: 'invalid', detail: '材料包至少包含一个文件' }
  }
  const invalidFile = request.files.find(
    (f) => !f.path || !isValidSha256(f.sha256) || !(f.sizeBytes > 0),
  )
  if (invalidFile) {
    return { kind: 'invalid', detail: `文件 ${invalidFile.path || '(未命名)'} 的 sha256 或大小不合法` }
  }

  const policy = getActivePolicy(courseId)
  if (!policy) return { kind: 'invalid', detail: `课程 ${courseId} 没有生效中的 LLM 出站策略` }
  if (policy.revision !== request.retentionPolicyRevision) {
    return { kind: 'policy-revision-mismatch' }
  }

  const sessionId = nextUuid7('upload')
  const expiresAt = addSecondsIso(900)
  const session: ProblemPackageUploadSessionSchema = {
    id: sessionId,
    courseId,
    revision: nextRevision(),
    expiresAt,
    files: request.files.map((f) => ({
      path: f.path,
      sizeBytes: f.sizeBytes,
      sha256: f.sha256,
      mediaType: f.mediaType,
    })),
    uploadTargets: request.files.map((f) => ({
      path: f.path,
      uploadUrl: `/api/v1/fixture-uploads/${sessionId}/${encodeURIComponent(f.path)}`,
      requiredHeaders: { 'x-fixture-upload': 'simulated' },
      expiresAt,
    })),
  }
  sessions.set(sessionId, session)
  sessionIdempotency.set(idempotencyKey, session)
  return { kind: 'ok', session }
}

export function completeUploadSession(
  courseId: string,
  uploadId: string,
  expectedRevision: number,
  manifestSha256: string,
  idempotencyKey: string,
): CompleteSessionResult {
  const cached = completeIdempotency.get(idempotencyKey)
  if (cached) return { kind: 'ok', package: cached }

  const session = sessions.get(uploadId)
  if (!session || session.courseId !== courseId) return { kind: 'not-found' }
  if (session.revision !== expectedRevision) return { kind: 'revision-mismatch' }
  if (!isValidSha256(manifestSha256)) {
    return { kind: 'invalid', detail: 'manifestSha256 不是合法的 SHA-256 摘要' }
  }

  const policy = getActivePolicy(courseId)
  const pkg: ProblemPackageSchema = {
    id: nextUuid7('pkg'),
    courseId,
    completedAt: nowIso(),
    manifestSha256,
    revision: nextRevision(),
    files: session.files.map((f) => ({
      path: f.path,
      object: {
        artifactId: nextUuid7('artifact'),
        mediaType: f.mediaType,
        objectVersion: '1',
        sha256: f.sha256,
        sizeBytes: f.sizeBytes,
        storeBinding: 'fixture-object-store',
      },
    })),
    retention: {
      class: 'course_material',
      disposition: 'delete',
      policyId: policy?.id ?? `policy-${courseId}`,
      policyRevision: policy?.revision ?? session.revision,
      retainUntil: addHoursIso(24 * 90),
    },
  }
  packages.set(pkg.id, pkg)
  completeIdempotency.set(idempotencyKey, pkg)
  return { kind: 'ok', package: pkg }
}

export function getProblemPackage(packageId: string): ProblemPackageSchema | undefined {
  return packages.get(packageId)
}

export function resetProblemPackageStore(): void {
  sessions.clear()
  packages.clear()
  sessionIdempotency.clear()
  completeIdempotency.clear()
}
