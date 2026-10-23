import { reactive, ref, watch, type Ref } from 'vue'
import {
  getOwnProjectEvaluationTaskForSubmission,
  getOwnProjectEvaluationTaskProgress,
  type EvaluationRunTaskProgress,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import type { SubmissionTaskFact } from '@/utils/evaluationProgress'

/**
 * Server reading of one evaluation task. A failed read keeps the last good
 * reading and reports the failure next to it, so a transient error never
 * replaces progress the server already confirmed.
 */
export interface EvaluationTaskRecord {
  fact: SubmissionTaskFact
  /** Failure of the most recent read; null after a successful read. */
  readError: TaskReadError | null
  /** When the shown fact was read, ISO time. */
  readAt: string | null
}

/** A read failure with the HTTP status the server answered, when it answered. */
export type TaskReadError = DiagnosticViewModel & { status?: number }

const UNREAD: EvaluationTaskRecord = { fact: { kind: 'unread' }, readError: null, readAt: null }

const READ_ERROR_REASONS: Record<string, string> = {
  LW_TASK_PROGRESS_RESOURCE_UNAVAILABLE: '资源服务暂时不可用，暂时读不到最新进度。',
  LW_TASK_PROGRESS_CHANGED: '进度正在变化，稍后会自动重新读取。',
}

function readError(error: unknown): TaskReadError {
  const problem = extractProblemDetails(error)
  const code = problem?.diagnosticCode ?? 'EVALUATION_TASK_LOAD_FAILED'
  const diagnostic: TaskReadError = makeDiagnostic(code, READ_ERROR_REASONS[code] ?? problem?.detail ?? '暂时读不到评测进度。', problem?.retryable ?? true)
  if (typeof problem?.status === 'number') diagnostic.status = problem.status
  return diagnostic
}

/**
 * Newer server facts win. Within one run the revision only grows; a different
 * run for the same submission is the one the server created later (UUIDv7
 * identifiers sort by creation time).
 */
function supersedes(next: EvaluationRunTaskProgress, shown: SubmissionTaskFact): boolean {
  if (shown.kind !== 'progress') return true
  const current = shown.progress
  if (next.runId !== current.runId) return next.runId > current.runId
  return next.revision >= current.revision
}

type ProgressResponse = { data?: EvaluationRunTaskProgress; error?: unknown }

/** Applies one response to a record; `allowNotCreated` treats 404 as "no run yet". */
function applyResponse(record: EvaluationTaskRecord, response: ProgressResponse, allowNotCreated: boolean): EvaluationTaskRecord {
  const now = new Date().toISOString()
  if (response.data) {
    if (!supersedes(response.data, record.fact)) return { ...record, readError: null }
    return { fact: { kind: 'progress', progress: response.data }, readError: null, readAt: now }
  }
  const problem = extractProblemDetails(response.error)
  if (allowNotCreated && problem?.status === 404 && problem.diagnosticCode === 'LW_EVALUATION_RUN_NOT_FOUND' && record.fact.kind !== 'progress') {
    return { fact: { kind: 'not-created' }, readError: null, readAt: now }
  }
  return { ...record, readError: readError(response.error) }
}

/**
 * Task progress for submissions in one project, keyed by submission. Each
 * submission resolves to the server's most recently created evaluation run.
 */
export function useSubmissionTasks(projectId: Ref<string | undefined>) {
  const records = ref<Record<string, EvaluationTaskRecord>>({})
  const inFlight = new Set<string>()
  let generation = 0

  async function readOne(project: string, submissionId: string, current: number) {
    let response: ProgressResponse
    try {
      response = await getOwnProjectEvaluationTaskForSubmission({ path: { projectId: project, submissionId } })
    } catch (error) {
      response = { error }
    }
    if (current !== generation || projectId.value !== project) return
    const record = records.value[submissionId] ?? UNREAD
    records.value = { ...records.value, [submissionId]: applyResponse(record, response, true) }
  }

  /** Re-read the given submissions; reads already in flight are not repeated. */
  async function refresh(submissionIds: readonly string[]) {
    const project = projectId.value
    if (!project) return
    const current = generation
    const ids = [...new Set(submissionIds)].filter((id) => !inFlight.has(id))
    for (let index = 0; index < ids.length; index += 4) {
      const batch = ids.slice(index, index + 4)
      batch.forEach((id) => inFlight.add(id))
      try {
        await Promise.all(batch.map((id) => readOne(project, id, current)))
      } finally {
        batch.forEach((id) => inFlight.delete(id))
      }
      if (current !== generation) return
    }
  }

  watch(projectId, () => {
    generation += 1
    records.value = {}
  })

  return reactive({ records, refresh })
}

/** Task progress of one evaluation run. */
export function useRunTask(projectId: Ref<string | undefined>, runId: Ref<string | undefined>) {
  const record = ref<EvaluationTaskRecord>(UNREAD)
  let generation = 0
  let reading: Promise<void> | null = null

  async function readOnce(project: string, run: string, current: number) {
    let response: ProgressResponse
    try {
      response = await getOwnProjectEvaluationTaskProgress({ path: { projectId: project, runId: run } })
    } catch (error) {
      response = { error }
    }
    if (current !== generation) return
    record.value = applyResponse(record.value, response, false)
  }

  function refresh(): Promise<void> {
    const project = projectId.value
    const run = runId.value
    if (!project || !run) return Promise.resolve()
    if (!reading) {
      reading = readOnce(project, run, generation).finally(() => { reading = null })
    }
    return reading
  }

  watch([projectId, runId], () => {
    generation += 1
    reading = null
    record.value = UNREAD
  })

  return reactive({ record, refresh })
}
