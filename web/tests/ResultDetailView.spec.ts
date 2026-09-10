import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createWebHistory } from 'vue-router'
import ResultDetailView from '@/views/student/ResultDetailView.vue'
import { getOwnProjectEvaluationResult, listProjects } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getOwnProjectEvaluationResult: vi.fn(),
    listProjects: vi.fn(),
  }
})

const project = {
  id: 'project-1',
  name: 'Review project',
  description: 'Goal review test project',
  ownerActorId: 'actor-1',
  courseId: null,
  revision: 1,
  state: 'active' as const,
  createdAt: '2026-09-08T00:00:00.000Z',
  updatedAt: '2026-09-08T00:00:00.000Z',
}

describe('ResultDetailView', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
    vi.mocked(listProjects).mockResolvedValue({ data: [project] as never, error: undefined as never })
    vi.mocked(getOwnProjectEvaluationResult).mockResolvedValue({
      data: {
        runId: 'run-1',
        projectId: 'project-1',
        courseId: null,
        releaseId: 'release-1',
        frozenSubmissionId: 'submission-1',
        revision: 1,
        state: 'succeeded',
        awardedScore: 8,
        maxScore: 10,
        createdAt: '2026-09-08T11:00:00.000Z',
        updatedAt: '2026-09-08T12:00:00.000Z',
        completedAt: '2026-09-08T12:00:00.000Z',
        steps: [
          {
            position: 0,
            role: 'advisory',
            state: 'succeeded',
            maxScore: 0,
            awardedScore: null,
            review: {
              schema_version: 'goal-review/v1',
              assessment: 'partially_met',
              confidence: 0.74,
              requires_teacher_attention: true,
              findings: [
                {
                  criterion: 'README explains the experiment',
                  result: 'partial',
                  suggestion: 'Add the observed result and the command used to reproduce it.',
                  evidence: [{ path: 'README.md', start_line: 4, end_line: 8 }],
                },
              ],
            },
          },
        ],
      } as never,
      error: undefined as never,
    })
  })

  it('renders advisory GoalReview findings and keeps them separate from the score', async () => {
    const router = createRouter({
      history: createWebHistory(),
      routes: [{ path: '/student/results/:runId', component: ResultDetailView }],
    })
    await router.push('/student/results/run-1')
    await router.isReady()

    const wrapper = mount(ResultDetailView, {
      global: { plugins: [router], stubs: { RouterLink: true } },
    })

    await vi.waitFor(() => expect(wrapper.text()).toContain('GoalReview · 目标建议'))
    expect(wrapper.text()).toContain('目标部分达成')
    expect(wrapper.text()).toContain('置信度')
    expect(wrapper.text()).toContain('74%')
    expect(wrapper.text()).toContain('README explains the experiment')
    expect(wrapper.text()).toContain('Add the observed result and the command used to reproduce it.')
    expect(wrapper.text()).toContain('README.md:4-8')
    expect(wrapper.text()).toContain('该建议需要教师关注')
    expect(wrapper.text()).toContain('这是评测建议，不计入确定性总分。')
    expect(wrapper.text()).toContain('8 / 10')
    expect(wrapper.find('.goal-review').text()).not.toContain('8 / 10')
    expect(getOwnProjectEvaluationResult).toHaveBeenCalledWith({
      path: { projectId: 'project-1', runId: 'run-1' },
    })
  })
})
