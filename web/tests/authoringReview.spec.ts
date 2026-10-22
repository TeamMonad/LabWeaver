import { describe, expect, it } from 'vitest'
import { activeReleasesByRun, queueGroup, releaseMatchesCandidate, stepSummaries } from '@/utils/authoringReview'
import { environmentCandidateView, evaluationCandidateView, reviewRunId, reviewRunRelease } from '../fixtures/authoring'

describe('authoringReview', () => {
  it('groups runs by what the teacher can do next', () => {
    expect(queueGroup('awaiting_approval')).toBe('review')
    expect(queueGroup('succeeded')).toBe('review')
    expect(queueGroup('running')).toBe('generating')
    expect(queueGroup('failed')).toBe('unfinished')
    // An active release is the server's record that the run was approved and published.
    expect(queueGroup('succeeded', true)).toBe('published')
    expect(queueGroup('awaiting_approval', true)).toBe('published')
  })

  it('keeps the latest active release per run and ignores withdrawn releases', () => {
    const withdrawn = { ...reviewRunRelease, id: 'release-withdrawn', version: 3, withdrawal: { actorId: 'a', reasonCode: 'TEACHER_WITHDRAWN', releaseId: 'release-withdrawn', releaseVersion: 3, withdrawnAt: '2026-10-12T00:00:00.000Z' } }
    const newer = { ...reviewRunRelease, id: 'release-newer', version: 2 }
    const otherRun = { ...reviewRunRelease, id: 'release-other', version: 9, agentRunId: 'run-other' }
    const byRun = activeReleasesByRun([reviewRunRelease, withdrawn, newer, otherRun])
    expect(byRun.get(reviewRunId)?.id).toBe('release-newer')
    expect(byRun.get('run-other')?.id).toBe('release-other')
    expect(activeReleasesByRun([withdrawn]).size).toBe(0)
  })

  it('treats a release of an earlier candidate revision as superseded', () => {
    const candidate = environmentCandidateView.candidate
    expect(releaseMatchesCandidate(reviewRunRelease, candidate)).toBe(true)
    expect(releaseMatchesCandidate(reviewRunRelease, { ...candidate, revision: candidate.revision + 1 })).toBe(false)
    expect(releaseMatchesCandidate(reviewRunRelease, { ...candidate, id: 'candidate-other' })).toBe(false)
  })

  it('summarises steps with role, points and failure behaviour', () => {
    const steps = stepSummaries(evaluationCandidateView.candidate.spec.spec)
    expect(steps.map((step) => [step.roleLabel, step.points])).toEqual([
      ['前置检查', '—'], ['计分', '60 分'], ['计分', '40 分'], ['AI 评语（不计分）', '—'],
    ])
    expect(steps[0].onFailure).toBe('不通过则停止评测')
  })
})
