import { describe, expect, it } from 'vitest'
import { queueGroup, stepSummaries, studentPreview } from '@/utils/authoringReview'
import { environmentCandidateView, evaluationCandidateView } from '../fixtures/authoring'

describe('authoringReview', () => {
  it('groups runs by what the teacher can do next', () => {
    expect(queueGroup('awaiting_approval')).toBe('review')
    expect(queueGroup('succeeded')).toBe('review')
    expect(queueGroup('running')).toBe('generating')
    expect(queueGroup('failed')).toBe('unfinished')
  })

  it('summarises steps with role, points and failure behaviour', () => {
    const steps = stepSummaries(evaluationCandidateView.candidate.spec.spec)
    expect(steps.map((step) => [step.roleLabel, step.points])).toEqual([
      ['前置检查', '—'], ['计分', '60 分'], ['计分', '40 分'], ['AI 评语（不计分）', '—'],
    ])
    expect(steps[0].onFailure).toBe('不通过则停止评测')
  })

  it('builds the student preview from an allow-list only', () => {
    const preview = studentPreview(environmentCandidateView.candidate.spec, evaluationCandidateView.candidate.spec.spec)
    const serialized = JSON.stringify(preview)
    expect(preview.requiredFiles).toEqual(['analysis.ipynb', 'results/fringe.csv'])
    expect(preview.gateCount).toBe(1)
    expect(serialized).not.toMatch(/hidden|testGroups|checker|"source"|条纹间距/)
  })
})
