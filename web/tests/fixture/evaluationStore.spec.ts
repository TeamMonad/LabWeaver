import { beforeEach, describe, expect, it } from 'vitest'
import { appendDecision } from '@/fixture/stores/approvalStore'
import { createEvaluationCandidate } from '@/fixture/stores/candidateStore'
import {
  createEvaluationRelease,
  getStudentResult,
  listEvaluationReleases,
  listStudentResults,
  resetEvaluationStore,
  seedEvaluationData,
  withdrawEvaluationRelease,
} from '@/fixture/stores/evaluationStore'

const sha = (character: string) => character.repeat(64)

function approvedCandidate() {
  const candidate = createEvaluationCandidate('fixture-run-test', 'course-101', 3, sha('a')).candidate
  const approval = appendDecision(candidate.id, {
    candidateRevision: candidate.revision,
    candidateSha256: candidate.specSha256,
    decision: 'approved',
    policyRevision: candidate.policyRevision,
    reason: 'approved for fixture test',
    schemaSha256: candidate.schemaSha256,
    trustRevision: 1,
  }, 'fixture-actor-teacher')
  return { candidate, approval }
}

describe('evaluationStore', () => {
  beforeEach(() => resetEvaluationStore())

  it('publishes idempotently and rejects key reuse with another payload', () => {
    const { candidate, approval } = approvedCandidate()
    const request = {
      approvalId: approval.id,
      candidateId: candidate.id,
      candidateRevision: candidate.revision,
      evaluationSpecSha256: candidate.specSha256,
    }
    const first = createEvaluationRelease('course-101', request, approval, 'fixture-actor-teacher', 'idem-publish')
    const replay = createEvaluationRelease('course-101', request, approval, 'fixture-actor-teacher', 'idem-publish')
    const conflict = createEvaluationRelease('course-102', request, approval, 'fixture-actor-teacher', 'idem-publish')
    expect(first.kind).toBe('ok')
    expect(replay).toEqual(first)
    expect(conflict.kind).toBe('conflict')
    expect(listEvaluationReleases('course-101')).toMatchObject({ items: [{ runtimeIdentity: { providerBinding: 'kubernetes/evaluation-primary-v1' } }] })
  })

  it('requires an exact approved candidate revision and hash', () => {
    const { candidate, approval } = approvedCandidate()
    const result = createEvaluationRelease('course-101', {
      approvalId: approval.id,
      candidateId: candidate.id,
      candidateRevision: candidate.revision + 1,
      evaluationSpecSha256: candidate.specSha256,
    }, approval, 'fixture-actor-teacher', 'idem-stale')
    expect(result.kind).toBe('precondition')
  })

  it('withdraws behind a revision fence and preserves historical results', () => {
    seedEvaluationData()
    const page = listEvaluationReleases('course-101')
    expect(page).not.toBe('invalid-cursor')
    if (page === 'invalid-cursor') return
    const release = page.items[0]
    const stale = withdrawEvaluationRelease('course-101', release.id, release.revision + 1, 'LW_TEST_WITHDRAW', 'idem-stale')
    const withdrawn = withdrawEvaluationRelease('course-101', release.id, release.revision, 'LW_TEST_WITHDRAW', 'idem-withdraw')
    expect(stale.kind).toBe('precondition')
    expect(withdrawn).toMatchObject({ kind: 'ok', release: { state: 'withdrawn', revision: release.revision + 1 } })
    expect(listStudentResults('course-101', 'fixture-actor-student')).toMatchObject({ items: { length: 3 } })
  })

  it('scopes terminal projections by course and actor and suppresses partial scores', () => {
    seedEvaluationData()
    const visible = listStudentResults('course-101', 'fixture-actor-student')
    expect(visible).not.toBe('invalid-cursor')
    if (visible === 'invalid-cursor') return
    expect(visible.items.map((result) => result.state).sort()).toEqual(['cancelled', 'failed', 'succeeded'])
    expect(visible.items.every((result) => result.steps.every((step) => step.position > 0))).toBe(true)
    for (const result of visible.items.filter((item) => item.state !== 'succeeded')) {
      expect(result.awardedScore).toBeUndefined()
      expect(result.steps.every((step) => step.awardedScore === undefined)).toBe(true)
    }
    expect(listStudentResults('course-102', 'fixture-actor-student')).toMatchObject({ items: [] })
    expect(listStudentResults('course-101', 'fixture-actor-teacher')).toMatchObject({ items: [] })
    expect(getStudentResult('course-101', 'other-actor', visible.items[0].runId)).toBeUndefined()
  })
})
