import { describe, expect, it } from 'vitest'
import { evaluationOutcome, submissionStages } from '@/utils/evaluationOutcome'
import type { StudentEvaluationResultSchema } from '@/generated/contracts'

const base = {
  runId: 'run-1', projectId: 'project-1', courseId: null, releaseId: 'release-1', frozenSubmissionId: 'submission-1', revision: 1,
  maxScore: 10, createdAt: '2026-09-08T11:00:00.000Z', updatedAt: '2026-09-08T12:00:00.000Z', completedAt: '2026-09-08T12:00:00.000Z', steps: [],
}
const result = (extra: Partial<StudentEvaluationResultSchema>) => ({ ...base, state: 'succeeded', ...extra }) as StudentEvaluationResultSchema
const time = (value: string) => value.slice(0, 10)

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
})

describe('submissionStages', () => {
  it('marks only confirmed stages before a result exists', () => {
    const stages = submissionStages({ frozenAt: '2026-09-08T10:00:00.000Z' }, time)
    expect(stages.map((stage) => stage.status)).toEqual(['done', 'unknown', 'unknown', 'unknown', 'current'])
    expect(stages[0].detail).toBe('保存于 2026-09-08')
  })

  it('shows the saving step as current until the snapshot is confirmed', () => {
    expect(submissionStages({}, time).map((stage) => stage.status)).toEqual(['current', 'unknown', 'unknown', 'unknown', 'upcoming'])
  })

  it('completes every stage for a finished run and fails only the result for an unfinished one', () => {
    expect(submissionStages({ frozenAt: base.createdAt, result: result({ awardedScore: 0 }) }, time).map((stage) => stage.status))
      .toEqual(['done', 'done', 'done', 'done', 'done'])
    expect(submissionStages({ frozenAt: base.createdAt, result: result({ state: 'failed', awardedScore: null }) }, time).map((stage) => stage.status))
      .toEqual(['done', 'unknown', 'unknown', 'unknown', 'failed'])
  })
})
