import type { StudentEvaluationResultSchema, StudentEvaluationStepResult } from '@/generated/contracts'

/**
 * Student-facing reading of one evaluation result.
 *
 * The run state from the server decides the outcome; diagnostic codes only add
 * a readable reason. A run that did not finish never shows a score, so a
 * platform failure can never look like a zero.
 */
export type EvaluationOutcomeKind = 'scored' | 'scored-zero' | 'not-completed' | 'cancelled'

export interface EvaluationOutcome {
  kind: EvaluationOutcomeKind
  /** Short badge text. */
  label: string
  /** One sentence: what happened. */
  summary: string
  /** One sentence: what the student can do next. */
  nextStep: string
  /** Readable reason from the diagnostic code, when the code is known. */
  reason: string | null
  /** Final deterministic score, only for a finished run. */
  score: { awarded: number; max: number } | null
}

/**
 * Codes produced by the student's own program. A finished run that carries one
 * of them is a normal, deterministic result.
 */
const PROGRAM_REASONS: Record<string, string> = {
  LW_OJ_ACCEPTED: '测试全部通过',
  LW_OJ_WRONG_ANSWER: '部分测试的输出与预期不一致',
  LW_OJ_COMPILE_ERROR: '程序编译失败',
  LW_OJ_TIME_LIMIT: '程序运行超过时间限制',
  LW_OJ_MEMORY_LIMIT: '程序运行超过内存限制',
  LW_OJ_OUTPUT_LIMIT: '程序输出超过长度限制',
  LW_OJ_RUNTIME_ERROR: '程序运行时出错',
}

/** Codes that mean the platform could not finish the evaluation. */
const PLATFORM_REASONS: Record<string, string> = {
  LW_OJ_INFRASTRUCTURE_ERROR: '评测平台执行出错',
  LW_OJ_JOB_FAILED: '评测任务没有正常结束',
  LW_OJ_JOB_DEADLINE_EXCEEDED: '评测任务超过平台时限',
  LW_OJ_NETWORK_ISOLATION_UNAVAILABLE: '评测所需的隔离环境暂不可用',
  LW_OJ_CLEANUP_UNKNOWN: '评测环境清理结果未知',
  LW_AP_INFRASTRUCTURE_ERROR: '评测平台执行出错',
  LW_EVALUATION_DATABASE_FAILED: '评测服务暂时无法保存结果',
  LW_EVALUATION_IO_FAILED: '评测服务读写失败',
  LW_EVALUATION_DEPENDENCY_FAILED: '评测依赖的服务暂不可用',
  LW_EVALUATION_STEP_LEASE_LOST: '评测执行被中断',
  LW_TASK_RESOURCE_APPROVAL_TIMEOUT: '评测资源审批超时',
  LW_EVALUATION_RELEASE_WITHDRAWN: '教师已撤回本次评测所用的版本',
  LW_EVALUATION_CANCELLED: '评测已取消',
  LW_OJ_JOB_CANCELLED: '评测已取消',
}

export function evaluationReason(code: string | null | undefined): string | null {
  if (!code) return null
  return PROGRAM_REASONS[code] ?? PLATFORM_REASONS[code] ?? null
}

function firstStepCode(result: StudentEvaluationResultSchema, predicate: (step: StudentEvaluationStepResult) => boolean): string | null {
  return result.steps.find((step) => predicate(step) && step.diagnosticCode)?.diagnosticCode ?? null
}

export function evaluationOutcome(result: StudentEvaluationResultSchema): EvaluationOutcome {
  if (result.state === 'succeeded' && typeof result.awardedScore === 'number') {
    const score = { awarded: result.awardedScore, max: result.maxScore }
    const reasonCode = firstStepCode(result, (step) => step.state === 'failed')
      ?? firstStepCode(result, (step) => step.role !== 'advisory' && step.diagnosticCode !== 'LW_OJ_ACCEPTED')
    if (result.awardedScore === 0) {
      return {
        kind: 'scored-zero',
        label: '评测完成 · 0 分',
        summary: '评测已正常完成，你的提交得到 0 分。',
        nextStep: '查看下方步骤了解未得分的原因，修改后重新提交。',
        reason: evaluationReason(reasonCode),
        score,
      }
    }
    return {
      kind: 'scored',
      label: '评测完成',
      summary: `评测已正常完成，得分 ${score.awarded} / ${score.max}。`,
      nextStep: '查看下方步骤了解各部分得分。',
      reason: evaluationReason(reasonCode),
      score,
    }
  }
  if (result.state === 'cancelled') {
    return {
      kind: 'cancelled',
      label: '已取消 · 不计分',
      summary: '这次评测已取消，没有产生分数，也不会记为 0 分。',
      nextStep: '如需成绩，请重新提交；有疑问请联系教师。',
      reason: evaluationReason(result.diagnosticCode),
      score: null,
    }
  }
  // failed, or a succeeded run without a score: the run did not finish.
  const code = result.diagnosticCode ?? firstStepCode(result, (step) => step.state === 'failed' || step.state === 'retryable')
  return {
    kind: 'not-completed',
    label: '评测未完成 · 不计分',
    summary: '评测没有完成，没有产生分数，这不是 0 分。',
    nextStep: '你的提交已保存。请复制诊断信息发给教师或管理员处理。',
    reason: evaluationReason(code),
    score: null,
  }
}

const STEP_ROLE: Record<StudentEvaluationStepResult['role'], string> = {
  gate: '前置检查',
  score: '评分',
  advisory: '学习建议',
}

const STEP_STATE: Record<StudentEvaluationStepResult['state'], string> = {
  pending: '等待执行',
  running: '执行中',
  retryable: '等待平台重试',
  succeeded: '已完成',
  failed: '未通过',
  cancelled: '已取消',
  skipped: '已跳过',
}

export function evaluationStepRoleLabel(role: StudentEvaluationStepResult['role']): string {
  return STEP_ROLE[role] ?? role
}

/**
 * A failed step in a finished run is the student's result (for example a
 * failed compile gate). A failed step in a run that did not finish is reported
 * as not completed, so it never reads as the student's mistake.
 */
export function evaluationStepStateLabel(step: StudentEvaluationStepResult, outcome: EvaluationOutcomeKind): string {
  if (step.state === 'failed' && (outcome === 'not-completed' || outcome === 'cancelled')) return '未完成'
  return STEP_STATE[step.state] ?? step.state
}

/** Progress stages of one submission, in order. */
export type SubmissionStageId = 'submitted' | 'approval' | 'queued' | 'running' | 'result'
export type SubmissionStageStatus = 'done' | 'current' | 'failed' | 'unknown' | 'upcoming'

export interface SubmissionStage {
  id: SubmissionStageId
  title: string
  status: SubmissionStageStatus
  detail: string
}

export interface SubmissionFacts {
  /** Frozen-at timestamp once the server confirms the snapshot. */
  frozenAt?: string | null
  /** Terminal result for this submission, when the server has one. */
  result?: StudentEvaluationResultSchema | null
}

/**
 * Builds the stage list only from what the server has confirmed. Approval,
 * queue and execution have no student-readable state yet, so before a result
 * they are shown as "not provided" instead of being guessed.
 */
export function submissionStages(facts: SubmissionFacts, formatTime: (value: string) => string): SubmissionStage[] {
  const result = facts.result ?? null
  const outcome = result ? evaluationOutcome(result) : null
  const finished = outcome?.kind === 'scored' || outcome?.kind === 'scored-zero'
  const middle = (id: SubmissionStageId, title: string): SubmissionStage => {
    if (finished) return { id, title, status: 'done', detail: '已完成' }
    if (result) return { id, title, status: 'unknown', detail: '平台未提供这一阶段的记录' }
    return { id, title, status: 'unknown', detail: '平台暂未提供这一阶段的实时状态' }
  }
  return [
    facts.frozenAt
      ? { id: 'submitted', title: '提交已保存', status: 'done', detail: `保存于 ${formatTime(facts.frozenAt)}` }
      : { id: 'submitted', title: '提交已保存', status: 'current', detail: '正在保存提交快照' },
    middle('approval', '等待审批'),
    middle('queued', '排队'),
    middle('running', '执行评测'),
    result && outcome
      ? {
          id: 'result',
          title: '结果',
          status: outcome.kind === 'scored' || outcome.kind === 'scored-zero' ? 'done' : 'failed',
          detail: `${outcome.label} · ${formatTime(result.completedAt)}`,
        }
      : facts.frozenAt
        ? { id: 'result', title: '结果', status: 'current', detail: '等待评测结果，页面会自动更新' }
        : { id: 'result', title: '结果', status: 'upcoming', detail: '提交保存后开始评测' },
  ]
}
