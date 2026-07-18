import type { AgentRunSchema, AgentTrack, CreateAgentRunRequestSchema } from '@/generated/contracts'
import { nextUuid7 } from '../utils/identity'
import { nextRevision } from '../utils/sequence'
import { getActivePolicy } from './llmPolicyStore'
import { getProblemPackage } from './problemPackageStore'
import * as candidateStore from './candidateStore'

/** Polls required before a running fixture run reaches its terminal state. */
const POLLS_TO_TERMINAL = 2

interface StoredAgentRun {
  run: AgentRunSchema
  polls: number
  /** When true, the run transitions to `failed` the first time it reaches a terminal state. */
  failsOnce?: boolean
}

const runs = new Map<string, StoredAgentRun>()
const createIdempotency = new Map<string, AgentRunSchema>()

export type CreateAgentRunResult =
  | { kind: 'ok'; run: AgentRunSchema }
  | { kind: 'package-not-found' }
  | { kind: 'revision-mismatch'; detail: string }

const FIXTURE_INPUT_SHA256 = 'c'.repeat(64)
const FIXTURE_OUTPUT_SHA256 = 'd'.repeat(64)

function buildTrack(kind: AgentTrack['kind'], state: AgentTrack['attempts'][number]['state']): AgentTrack {
  return {
    kind,
    candidateId: null,
    attempts: [
      {
        number: 1,
        state,
        inputSha256: FIXTURE_INPUT_SHA256,
        outputSha256: state === 'succeeded' ? FIXTURE_OUTPUT_SHA256 : null,
        diagnosticCode: null,
        usage: { inputTokens: 0, outputTokens: 0, requests: 0, costMicrousd: 0 },
        usageObserved: state === 'succeeded',
      },
    ],
  }
}

export function createAgentRun(
  courseId: string,
  request: CreateAgentRunRequestSchema,
  idempotencyKey: string,
): CreateAgentRunResult {
  const cached = createIdempotency.get(idempotencyKey)
  if (cached) return { kind: 'ok', run: cached }

  const pkg = getProblemPackage(request.packageId)
  if (!pkg || pkg.courseId !== courseId) return { kind: 'package-not-found' }
  if (pkg.revision !== request.packageRevision) {
    return { kind: 'revision-mismatch', detail: '材料包 revision 已变化，请刷新后重试' }
  }
  if (pkg.manifestSha256 !== request.packageSha256) {
    return { kind: 'revision-mismatch', detail: '材料包摘要与归档内容不一致' }
  }

  const policy = getActivePolicy(courseId)
  if (!policy || policy.id !== request.policyId || policy.revision !== request.policyRevision) {
    return { kind: 'revision-mismatch', detail: 'LLM 出站策略 revision 已变化，请刷新后重试' }
  }

  // Deterministic failure scenario: if the uploaded package contains a file
  // whose path includes `__run-fail__`, the run fails once and can be retried.
  const failsOnce = pkg.files.some((f) => f.path.includes('__run-fail__'))

  const environmentCandidateId = nextUuid7('candidate')
  const evaluationCandidateId = nextUuid7('candidate')

  const run: AgentRunSchema = {
    id: nextUuid7('run'),
    courseId,
    packageId: pkg.id,
    policyId: policy.id,
    policyRevision: policy.revision,
    requestedRuntime: request.requestedRuntime,
    revision: nextRevision(),
    state: 'running',
    tracks: [
      { ...buildTrack('environment', 'running'), candidateId: environmentCandidateId },
      { ...buildTrack('evaluation', 'running'), candidateId: evaluationCandidateId },
    ],
  }
  runs.set(run.id, { run, polls: 0, failsOnce })
  createIdempotency.set(idempotencyKey, run)
  return { kind: 'ok', run }
}

/** Deterministic progression: running runs succeed after a fixed number of polls. */
function advance(stored: StoredAgentRun): void {
  const { run } = stored
  if (run.state === 'cancelling') {
    run.state = 'cancelled'
    run.revision = nextRevision()
    return
  }
  if (run.state !== 'running') return
  stored.polls += 1
  if (stored.polls >= POLLS_TO_TERMINAL) {
    if (stored.failsOnce) {
      run.state = 'failed'
      run.revision = nextRevision()
      run.tracks = [buildTrack('environment', 'failed'), buildTrack('evaluation', 'failed')]
      return
    }
    run.state = 'succeeded'
    run.revision = nextRevision()
    const envTrack = run.tracks.find((t) => t.kind === 'environment')
    const evalTrack = run.tracks.find((t) => t.kind === 'evaluation')
    if (envTrack?.candidateId && !candidateStore.getEnvironmentCandidate(envTrack.candidateId)) {
      candidateStore.createEnvironmentCandidate(
        run.id,
        run.courseId,
        run.requestedRuntime,
        run.policyRevision,
        'sha256:' + 's'.repeat(64),
        envTrack.candidateId,
      )
    }
    if (evalTrack?.candidateId && !candidateStore.getEvaluationCandidate(evalTrack.candidateId)) {
      candidateStore.createEvaluationCandidate(
        run.id,
        run.courseId,
        run.policyRevision,
        'sha256:' + 's'.repeat(64),
        evalTrack.candidateId,
      )
    }
    run.tracks = [
      { ...buildTrack('environment', 'succeeded'), candidateId: envTrack?.candidateId ?? null },
      { ...buildTrack('evaluation', 'succeeded'), candidateId: evalTrack?.candidateId ?? null },
    ]
  }
}

export function getAgentRun(courseId: string, runId: string): AgentRunSchema | undefined {
  const stored = runs.get(runId)
  if (!stored || stored.run.courseId !== courseId) return undefined
  advance(stored)
  return stored.run
}

export function cancelAgentRun(
  courseId: string,
  runId: string,
  expectedRevision: number,
): AgentRunSchema | 'revision-mismatch' | 'not-cancellable' | undefined {
  const stored = runs.get(runId)
  if (!stored || stored.run.courseId !== courseId) return undefined
  if (stored.run.revision !== expectedRevision) return 'revision-mismatch'
  if (stored.run.state !== 'running' && stored.run.state !== 'requested') return 'not-cancellable'
  stored.run.state = 'cancelling'
  stored.run.revision = nextRevision()
  return stored.run
}

export function retryAgentRunTrack(
  courseId: string,
  runId: string,
  track: 'environment' | 'evaluation',
  expectedRevision: number,
): AgentRunSchema | 'revision-mismatch' | 'not-retryable' | undefined {
  const stored = runs.get(runId)
  if (!stored || stored.run.courseId !== courseId) return undefined
  if (stored.run.revision !== expectedRevision) return 'revision-mismatch'
  if (stored.run.state !== 'failed' && stored.run.state !== 'partially_succeeded') {
    return 'not-retryable'
  }
  const target = stored.run.tracks.find((t) => t.kind === track)
  if (!target) return 'not-retryable'
  target.attempts.push({
    number: target.attempts.length + 1,
    state: 'running',
    inputSha256: FIXTURE_INPUT_SHA256,
    outputSha256: null,
    diagnosticCode: null,
    usage: { inputTokens: 0, outputTokens: 0, requests: 0, costMicrousd: 0 },
    usageObserved: false,
  })
  stored.run.state = 'running'
  stored.run.revision = nextRevision()
  stored.polls = 0
  // Retry clears the one-shot failure flag so the run can eventually succeed.
  stored.failsOnce = false
  return stored.run
}

export function resetAgentRunStore(): void {
  runs.clear()
  createIdempotency.clear()
}
