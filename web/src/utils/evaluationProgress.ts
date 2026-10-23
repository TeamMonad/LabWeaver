import type {
  EvaluationRunTaskProgress,
  EvaluationStepTaskProgress,
  StudentEvaluationResultSchema,
  TaskAttemptPhase,
} from '@/generated/contracts'
import { evaluationOutcome, evaluationReason, isResourceStopCode } from '@/utils/evaluationOutcome'

/**
 * Student-facing reading of one submission's evaluation task.
 *
 * Everything here comes from the server's task progress for the submission's
 * current evaluation run: the run state, the current attempt of each step, and
 * the bounded history of earlier attempts. Earlier attempts are only counted;
 * they never decide the stage, so an old failure cannot cover new progress.
 * There is no percentage: the server does not report how much work is left.
 */

/** What the server has said about the evaluation task of one submission. */
export type SubmissionTaskFact =
  /** Not read yet, or every read so far failed. */
  | { kind: 'unread' }
  /** The server has no evaluation run for this submission yet. */
  | { kind: 'not-created' }
  | { kind: 'progress'; progress: EvaluationRunTaskProgress }

export type SubmissionStageId = 'submitted' | 'queued' | 'approval' | 'running' | 'result'
export type SubmissionStageStatus = 'done' | 'current' | 'failed' | 'unknown' | 'upcoming'

export interface SubmissionStage {
  id: SubmissionStageId
  title: string
  status: SubmissionStageStatus
  detail: string
}

type TaskStageId = Exclude<SubmissionStageId, 'submitted'>

export interface EvaluationTaskReading {
  /** The stage the run is in now. */
  stage: TaskStageId
  status: 'current' | 'failed'
  /** Short chip text. */
  label: string
  /** One sentence for the current stage. */
  detail: string
  /** Readable reason from the current attempt's diagnostic code, when known. */
  reason: string | null
  diagnosticCode: string | null
  /** Step being worked on (1-based), only when the run has several steps. */
  step: { position: number; count: number } | null
  /** Attempt number of the step being worked on, once it has been claimed. */
  attempt: number | null
  /** Earlier attempts in the server's first history page. */
  earlierAttempts: { count: number; more: boolean }
  /** The step being worked on is past approval and never needed a Resource request. */
  approvalNotNeeded: boolean
  /** The run itself has ended; the result is read separately. */
  terminal: boolean
}

const TERMINAL_RUN_STATES: Record<EvaluationRunTaskProgress['state'], boolean> = {
  queued: false,
  running: false,
  cancelling: false,
  succeeded: true,
  failed: true,
  cancelled: true,
}

export function isTerminalRun(progress: EvaluationRunTaskProgress): boolean {
  return TERMINAL_RUN_STATES[progress.state] === true
}

const PHASE_READING: Partial<Record<TaskAttemptPhase, { stage: TaskStageId; label: string; detail: string }>> = {
  queued: { stage: 'queued', label: '排队中', detail: '评测已创建，正在排队等待执行。' },
  preparing: { stage: 'queued', label: '准备中', detail: '评测执行器已接手，正在准备执行。' },
  waiting_resource_approval: { stage: 'approval', label: '等待资源审批', detail: '评测所需资源正在等待审批，批准后自动开始。' },
  allocating: { stage: 'approval', label: '分配资源中', detail: '资源已批准，正在分配。' },
  executing: { stage: 'running', label: '评测执行中', detail: '正在运行评测。' },
  awaiting_review: { stage: 'running', label: '等待复核', detail: '评测已执行，正在等待复核。' },
  cleanup_pending: { stage: 'running', label: '清理中', detail: '正在清理评测环境，清理确认后继续。' },
}

/** A step the run is no longer working on: finished, cancelled, or skipped. */
function isSettled(step: EvaluationStepTaskProgress): boolean {
  const { phase, resourceState } = step.progress
  if (phase === 'succeeded' || phase === 'cancelled') return true
  // A skipped step is reported as blocked without any Resource request state.
  return phase === 'blocked' && !resourceState
}

export function readEvaluationTask(progress: EvaluationRunTaskProgress): EvaluationTaskReading {
  const base = {
    reason: null,
    diagnosticCode: null,
    step: null,
    attempt: null,
    earlierAttempts: { count: progress.history.length, more: progress.hasMore },
    approvalNotNeeded: false,
    terminal: false,
  } satisfies Partial<EvaluationTaskReading>

  if (isTerminalRun(progress)) {
    return { ...base, stage: 'result', status: 'current', label: '已结束', detail: '评测已结束，正在读取结果。', terminal: true }
  }
  if (progress.state === 'cancelling') {
    return { ...base, stage: 'result', status: 'current', label: '正在取消', detail: '评测正在取消，取消后不计分。' }
  }

  const index = progress.current.findIndex((step) => !isSettled(step))
  if (index < 0) {
    return { ...base, stage: 'result', status: 'current', label: '汇总结果中', detail: '评测步骤已执行完，正在汇总结果。' }
  }
  const attempt = progress.current[index].progress
  const code = attempt.diagnosticCode ?? null
  const active = {
    ...base,
    reason: evaluationReason(code),
    diagnosticCode: code,
    step: progress.current.length > 1 ? { position: index + 1, count: progress.current.length } : null,
    attempt: attempt.attempt > 0 ? attempt.attempt : null,
  }

  if (attempt.phase === 'failed') {
    // The run is still open, so this step will be retried or the run will end
    // as not completed. Neither is the student's score.
    return {
      ...active,
      stage: 'queued',
      status: 'current',
      label: '尝试未完成',
      detail: '这一步的上一次尝试没有完成，平台正在处理；可重试的故障会自动重试。这不会记为 0 分。',
    }
  }
  if (attempt.phase === 'blocked') {
    return {
      ...active,
      stage: 'approval',
      status: 'failed',
      label: '资源未就绪',
      detail: '评测所需资源没有就绪，这次评测将以“未完成”结束，不计分。',
    }
  }
  const reading = PHASE_READING[attempt.phase]
  if (!reading) {
    return { ...active, stage: 'result', status: 'current', label: '汇总结果中', detail: '评测步骤已执行完，正在汇总结果。' }
  }
  return {
    ...active,
    ...reading,
    status: 'current',
    approvalNotNeeded: reading.stage === 'running' && !attempt.resourceRequestId,
  }
}

export interface SubmissionFacts {
  /** Frozen-at timestamp once the server confirms the snapshot. */
  frozenAt?: string | null
  /** Server task progress for the submission's evaluation. */
  task?: SubmissionTaskFact
  /** Terminal result for this submission, when the server has one. */
  result?: StudentEvaluationResultSchema | null
  /** The run ended but its result could not be read and will not be re-read automatically. */
  resultUnreadable?: boolean
}

const TASK_STAGES: Array<{ id: TaskStageId; title: string }> = [
  { id: 'queued', title: '排队与准备' },
  { id: 'approval', title: '资源审批' },
  { id: 'running', title: '执行评测' },
  { id: 'result', title: '结果' },
]

function finishedStages(result: StudentEvaluationResultSchema, formatTime: (value: string) => string): SubmissionStage[] {
  const outcome = evaluationOutcome(result)
  const scored = outcome.kind === 'scored' || outcome.kind === 'scored-zero'
  const code = result.diagnosticCode ?? null
  const stoppedAtApproval = !scored && isResourceStopCode(code)
  return TASK_STAGES.map(({ id, title }): SubmissionStage => {
    if (id === 'result') {
      return { id, title, status: scored ? 'done' : 'failed', detail: `${outcome.label} · ${formatTime(result.completedAt)}` }
    }
    if (scored) return { id, title, status: 'done', detail: '已完成' }
    if (stoppedAtApproval) {
      if (id === 'queued') return { id, title, status: 'done', detail: '已完成' }
      if (id === 'approval') return { id, title, status: 'failed', detail: evaluationReason(code) ?? '资源没有就绪' }
      return { id, title, status: 'upcoming', detail: '未开始' }
    }
    return { id, title, status: 'unknown', detail: '评测未完成，平台未提供停在哪一阶段' }
  })
}

function readingStages(reading: EvaluationTaskReading, resultUnreadable: boolean): SubmissionStage[] {
  const currentIndex = TASK_STAGES.findIndex((stage) => stage.id === reading.stage)
  return TASK_STAGES.map(({ id, title }, index): SubmissionStage => {
    if (reading.terminal) {
      if (id !== 'result') return { id, title, status: 'unknown', detail: resultUnreadable ? '结果无法读取，平台未提供这一阶段的记录' : '正在读取结果' }
      return resultUnreadable
        ? { id, title, status: 'failed', detail: '评测已结束，但无法读取结果' }
        : { id, title, status: 'current', detail: reading.detail }
    }
    if (index < currentIndex) {
      if (id === 'approval' && reading.approvalNotNeeded) return { id, title, status: 'done', detail: '这一步无需资源审批' }
      return { id, title, status: 'done', detail: '已完成' }
    }
    if (index === currentIndex) {
      const reason = reading.status === 'failed' && reading.reason ? `原因：${reading.reason}。` : ''
      const step = reading.step ? `第 ${reading.step.position} 步（共 ${reading.step.count} 步）：` : ''
      return { id, title, status: reading.status, detail: `${step}${reading.detail}${reason}` }
    }
    if (id === 'result') return { id, title, status: 'upcoming', detail: '评测完成后显示，页面会自动更新' }
    return { id, title, status: 'upcoming', detail: '未开始' }
  })
}

/**
 * Builds the stage list only from what the server has confirmed. A stage the
 * server has not reported is shown as unknown, never guessed.
 */
export function submissionStages(facts: SubmissionFacts, formatTime: (value: string) => string): SubmissionStage[] {
  const submitted: SubmissionStage = facts.frozenAt
    ? { id: 'submitted', title: '提交已保存', status: 'done', detail: `保存于 ${formatTime(facts.frozenAt)}` }
    : { id: 'submitted', title: '提交已保存', status: 'current', detail: '正在读取提交快照' }
  if (facts.result) return [submitted, ...finishedStages(facts.result, formatTime)]

  const task = facts.task ?? { kind: 'unread' }
  if (task.kind === 'progress') return [submitted, ...readingStages(readEvaluationTask(task.progress), Boolean(facts.resultUnreadable))]
  if (task.kind === 'not-created') {
    return [
      submitted,
      ...TASK_STAGES.map(({ id, title }): SubmissionStage => id === 'queued'
        ? { id, title, status: 'current', detail: '平台还没有为这次提交创建评测，创建后自动显示。' }
        : { id, title, status: 'upcoming', detail: id === 'result' ? '评测完成后显示，页面会自动更新' : '未开始' }),
    ]
  }
  return [
    submitted,
    ...TASK_STAGES.map(({ id, title }): SubmissionStage => id === 'result'
      ? { id, title, status: facts.frozenAt ? 'current' : 'upcoming', detail: '等待评测结果，页面会自动更新' }
      : { id, title, status: 'unknown', detail: '暂时无法读取这一阶段的状态' }),
  ]
}

/** Short chip text for a submission that has no result yet. */
export function pendingTaskLabel(task: SubmissionTaskFact): string {
  if (task.kind === 'progress') return readEvaluationTask(task.progress).label
  if (task.kind === 'not-created') return '等待创建评测'
  return '等待结果'
}
