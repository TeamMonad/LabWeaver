import { describe, expect, it } from 'vitest'
import type { EvaluationRunTaskProgress, StudentEvaluationResultSchema, TaskAttemptProgress } from '@/generated/contracts'
import { pendingTaskLabel, readEvaluationTask, submissionStages } from '@/utils/evaluationProgress'

const time = (value: string) => value.slice(0, 10)

function attempt(extra: Partial<TaskAttemptProgress>): TaskAttemptProgress {
  return { attempt: 1, phase: 'queued', nextSteps: ['getOwnProjectEvaluationTaskProgress'], ...extra }
}

function progress(steps: Array<Partial<TaskAttemptProgress>>, extra: Partial<EvaluationRunTaskProgress> = {}): EvaluationRunTaskProgress {
  return {
    runId: 'run-1',
    projectId: 'project-1',
    revision: 3,
    state: 'running',
    current: steps.map((step, index) => ({ stepRunId: `step-${index}`, progress: attempt(step) })),
    history: [],
    page: 1,
    pageSize: 20,
    hasMore: false,
    ...extra,
  }
}

const result = (extra: Partial<StudentEvaluationResultSchema>) => ({
  runId: 'run-1', projectId: 'project-1', courseId: null, releaseId: 'release-1', frozenSubmissionId: 'submission-1', revision: 1,
  maxScore: 10, createdAt: '2026-09-08T11:00:00.000Z', updatedAt: '2026-09-08T12:00:00.000Z', completedAt: '2026-09-08T12:00:00.000Z',
  steps: [], state: 'succeeded', ...extra,
}) as StudentEvaluationResultSchema

const statuses = (facts: Parameters<typeof submissionStages>[0]) => submissionStages(facts, time).map((stage) => stage.status)
const frozenAt = '2026-09-08T10:00:00.000Z'

describe('readEvaluationTask', () => {
  it('follows the first step that is still being worked on', () => {
    const reading = readEvaluationTask(progress([
      { phase: 'succeeded' },
      { phase: 'waiting_resource_approval', resourceRequestId: 'request-1', resourceState: 'reviewing' },
      { phase: 'queued', attempt: 0 },
    ]))
    expect(reading).toMatchObject({ stage: 'approval', status: 'current', label: '等待资源审批', step: { position: 2, count: 3 } })
  })

  it('reads a failed attempt in an open run as being handled, not as the student result', () => {
    const reading = readEvaluationTask(progress([{ phase: 'failed', diagnosticCode: 'LW_OJ_INFRASTRUCTURE_ERROR' }]))
    expect(reading.status).toBe('current')
    expect(reading.detail).toContain('不会记为 0 分')
    expect(reading.reason).toBe('评测平台执行出错')
  })

  it('reports a step blocked on resources as failed with its reason first', () => {
    const reading = readEvaluationTask(progress([{ phase: 'blocked', resourceState: 'rejected', diagnosticCode: 'LW_TASK_RESOURCE_APPROVAL_TIMEOUT' }]))
    expect(reading).toMatchObject({ stage: 'approval', status: 'failed', reason: '评测所需资源的审批超时', diagnosticCode: 'LW_TASK_RESOURCE_APPROVAL_TIMEOUT' })
  })

  it('counts earlier attempts without letting them decide the current stage', () => {
    const reading = readEvaluationTask(progress(
      [{ phase: 'executing', attempt: 2, resourceRequestId: 'request-2', resourceState: 'active' }],
      { history: [{ stepRunId: 'step-0', progress: attempt({ phase: 'failed', attempt: 1, diagnosticCode: 'LW_OJ_JOB_FAILED' }) }], hasMore: true },
    ))
    expect(reading).toMatchObject({ stage: 'running', status: 'current', attempt: 2, earlierAttempts: { count: 1, more: true }, reason: null })
  })

  it('marks a terminal run as ended so its result is read next', () => {
    expect(readEvaluationTask(progress([{ phase: 'succeeded' }], { state: 'succeeded' }))).toMatchObject({ stage: 'result', terminal: true })
  })
})

describe('submissionStages', () => {
  it('marks every task stage unknown while progress cannot be read', () => {
    expect(statuses({ frozenAt })).toEqual(['done', 'unknown', 'unknown', 'unknown', 'current'])
    expect(submissionStages({ frozenAt }, time)[0].detail).toBe('保存于 2026-09-08')
  })

  it('shows the saving step as current until the snapshot is confirmed', () => {
    expect(statuses({})).toEqual(['current', 'unknown', 'unknown', 'unknown', 'upcoming'])
  })

  it('places the current stage from server progress and marks only earlier stages done', () => {
    const approval = progress([{ phase: 'waiting_resource_approval', resourceRequestId: 'request-1', resourceState: 'reviewing' }])
    expect(statuses({ frozenAt, task: { kind: 'progress', progress: approval } })).toEqual(['done', 'done', 'current', 'upcoming', 'upcoming'])
    const executing = progress([{ phase: 'executing', resourceRequestId: 'request-1', resourceState: 'active' }])
    expect(statuses({ frozenAt, task: { kind: 'progress', progress: executing } })).toEqual(['done', 'done', 'done', 'current', 'upcoming'])
  })

  it('says approval was not needed for a step that ran without a resource request', () => {
    const stages = submissionStages({ frozenAt, task: { kind: 'progress', progress: progress([{ phase: 'executing' }]) } }, time)
    expect(stages.find((stage) => stage.id === 'approval')?.detail).toBe('这一步无需资源审批')
  })

  it('shows a run the server has not created yet as waiting in the queue', () => {
    expect(statuses({ frozenAt, task: { kind: 'not-created' } })).toEqual(['done', 'current', 'upcoming', 'upcoming', 'upcoming'])
    expect(pendingTaskLabel({ kind: 'not-created' })).toBe('等待创建评测')
  })

  it('never shows a percentage', () => {
    const stages = submissionStages({ frozenAt, task: { kind: 'progress', progress: progress([{ phase: 'executing' }]) } }, time)
    expect(stages.map((stage) => stage.detail).join(' ')).not.toMatch(/%/)
  })

  it('completes every stage for a scored run, including a deterministic zero', () => {
    expect(statuses({ frozenAt, result: result({ awardedScore: 0 }) })).toEqual(['done', 'done', 'done', 'done', 'done'])
  })

  it('stops at approval for a run that ended because resources were never approved', () => {
    const stages = submissionStages({ frozenAt, result: result({ state: 'failed', awardedScore: null, diagnosticCode: 'LW_TASK_RESOURCE_APPROVAL_TIMEOUT' }) }, time)
    expect(stages.map((stage) => stage.status)).toEqual(['done', 'done', 'failed', 'upcoming', 'failed'])
    expect(stages[2].detail).toBe('评测所需资源的审批超时')
  })

  it('does not invent the stage where another unfinished run stopped', () => {
    expect(statuses({ frozenAt, result: result({ state: 'failed', awardedScore: null }) })).toEqual(['done', 'unknown', 'unknown', 'unknown', 'failed'])
  })

  it('waits on the result of an ended run until it is read', () => {
    const ended = { kind: 'progress' as const, progress: progress([{ phase: 'succeeded' }], { state: 'succeeded' }) }
    const stages = submissionStages({ frozenAt, task: ended }, time)
    expect(stages.map((stage) => stage.status)).toEqual(['done', 'unknown', 'unknown', 'unknown', 'current'])
    expect(stages[1].detail).toBe('正在读取结果')
  })

  it('marks the result failed instead of loading when an ended run\'s result cannot be read', () => {
    const ended = { kind: 'progress' as const, progress: progress([{ phase: 'succeeded' }], { state: 'succeeded' }) }
    const stages = submissionStages({ frozenAt, task: ended, resultUnreadable: true }, time)
    expect(stages.map((stage) => stage.status)).toEqual(['done', 'unknown', 'unknown', 'unknown', 'failed'])
    expect(stages.map((stage) => stage.detail).join('')).not.toContain('正在读取结果')
    expect(stages[4].detail).toBe('评测已结束，但无法读取结果')
  })
})
