import { describe, expect, it } from 'vitest'
import { evaluationOutcome } from '@/utils/evaluationOutcome'
import type { StudentEvaluationResultSchema } from '@/generated/contracts'

const base = {
  runId: 'run-1', projectId: 'project-1', courseId: null, releaseId: 'release-1', frozenSubmissionId: 'submission-1', revision: 1,
  maxScore: 10, createdAt: '2026-09-08T11:00:00.000Z', updatedAt: '2026-09-08T12:00:00.000Z', completedAt: '2026-09-08T12:00:00.000Z', steps: [],
}
const result = (extra: Partial<StudentEvaluationResultSchema>) => ({ ...base, state: 'succeeded', ...extra }) as StudentEvaluationResultSchema

describe('evaluationOutcome', () => {
  it('treats a succeeded run without a final score as not completed, never as zero', () => {
    const outcome = evaluationOutcome(result({ awardedScore: null }))
    expect(outcome.kind).toBe('not-completed')
    expect(outcome.score).toBeNull()
  })

  it('keeps the score of a finished run and reads the first failing step as the reason', () => {
    const outcome = evaluationOutcome(result({
      awardedScore: 4,
      steps: [
        { position: 0, role: 'gate', state: 'succeeded', maxScore: 0, diagnosticCode: 'LW_OJ_ACCEPTED' },
        { position: 1, role: 'score', state: 'succeeded', maxScore: 10, awardedScore: 4, diagnosticCode: 'LW_OJ_TIME_LIMIT' },
      ],
    }))
    expect(outcome).toMatchObject({ kind: 'scored', score: { awarded: 4, max: 10 }, reason: '程序运行超过时间限制' })
  })

  it('leaves unknown diagnostic codes without an invented reason', () => {
    expect(evaluationOutcome(result({ state: 'failed', awardedScore: null, diagnosticCode: 'LW_SOMETHING_NEW' })).reason).toBeNull()
  })

  it('tells the student to resubmit when the evaluation stopped because resources were never approved', () => {
    const outcome = evaluationOutcome(result({ state: 'failed', awardedScore: null, diagnosticCode: 'LW_TASK_RESOURCE_APPROVAL_TIMEOUT' }))
    expect(outcome).toMatchObject({ kind: 'not-completed', score: null, reason: '评测所需资源的审批超时' })
    expect(outcome.nextStep).toContain('重新提交')
  })
})
