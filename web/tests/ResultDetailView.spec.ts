import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createWebHistory } from 'vue-router'
import ResultDetailView from '@/views/student/ResultDetailView.vue'
import {
  getEnvironment,
  getFrozenSubmission,
  getOwnProjectEvaluationResult,
  getOwnProjectEvaluationTaskForSubmission,
  getOwnProjectEvaluationTaskProgress,
  listProjects,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getOwnProjectEvaluationResult: vi.fn(),
    getOwnProjectEvaluationTaskProgress: vi.fn(),
    getOwnProjectEvaluationTaskForSubmission: vi.fn(),
    getFrozenSubmission: vi.fn(),
    getEnvironment: vi.fn(),
    listProjects: vi.fn(),
  }
})

const notFinished = { data: undefined, error: { status: 404, diagnosticCode: 'LW_EVALUATION_RUN_NOT_FOUND', detail: 'missing' } } as never

function runProgress(phase: string, extra: Record<string, unknown> = {}, attempt: Record<string, unknown> = {}) {
  return {
    data: {
      runId: 'run-x', projectId: 'project-1', revision: 2, state: 'running',
      current: [{ stepRunId: 'step-1', progress: { attempt: 1, phase, nextSteps: ['getOwnProjectEvaluationTaskProgress'], ...attempt } }],
      history: [], page: 1, pageSize: 20, hasMore: false, ...extra,
    },
    error: undefined,
  } as never
}

async function mountRun(query: Record<string, string> = { projectId: 'project-1' }) {
  const router = createRouter({
    history: createWebHistory(),
    routes: [
      { path: '/student/results', component: { template: '<div />' } },
      { path: '/student/results/:runId', component: ResultDetailView },
    ],
  })
  await router.push({ path: '/student/results/run-x', query })
  await router.isReady()
  return mount(ResultDetailView, { global: { plugins: [router] } })
}

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

const projectB = {
  ...project,
  id: 'project-2',
  name: 'Second project',
}

describe('ResultDetailView', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
    vi.mocked(listProjects).mockResolvedValue({ data: [project] as never, error: undefined as never })
    vi.mocked(getFrozenSubmission).mockImplementation(async (options) => ({
      data: {
        id: options.path.submissionId,
        projectId: options.path.projectId,
        frozenAt: '2026-09-08T10:30:00.000Z',
        environment: { environmentId: 'env-1', environmentRevision: 1, releaseId: 'tpl-1', releaseVersion: 1, runtimeKind: 'container' },
      },
      error: undefined,
    }) as never)
    vi.mocked(getEnvironment).mockResolvedValue({ data: { id: 'env-1', displayLabel: '量子隧穿实验' }, error: undefined } as never)
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

    await vi.waitFor(() => expect(wrapper.text()).toContain('学习建议 · 目标部分达成'))
    expect(wrapper.text()).toContain('置信度 74%')
    expect(wrapper.text()).toContain('README explains the experiment')
    expect(wrapper.text()).toContain('Add the observed result and the command used to reproduce it.')
    expect(wrapper.text()).toContain('README.md')
    expect(wrapper.text()).toContain('第 4–8 行')
    expect(wrapper.text()).toContain('需要教师结合提交内容人工复核')
    expect(wrapper.find('.review-disclaimer').text()).toContain('不计入总分')
    expect(wrapper.text()).toContain('8 / 10')
    expect(wrapper.find('.goal-review').text()).not.toContain('8 / 10')
    expect(getOwnProjectEvaluationResult).toHaveBeenCalledWith({
      path: { projectId: 'project-1', runId: 'run-1' },
    })
  })

  it('keeps a direct project route context for detail loading and the back link', async () => {
    vi.mocked(listProjects).mockResolvedValue({
      data: [project, projectB] as never,
      error: undefined as never,
    })
    vi.mocked(getOwnProjectEvaluationResult).mockResolvedValue({
      data: {
        runId: 'run-2',
        projectId: 'project-2',
        courseId: null,
        releaseId: 'release-2',
        frozenSubmissionId: 'submission-2',
        revision: 1,
        state: 'succeeded',
        awardedScore: 10,
        maxScore: 10,
        createdAt: '2026-09-08T11:00:00.000Z',
        updatedAt: '2026-09-08T12:00:00.000Z',
        completedAt: '2026-09-08T12:00:00.000Z',
        steps: [],
      } as never,
      error: undefined as never,
    })
    const router = createRouter({
      history: createWebHistory(),
      routes: [
        { path: '/student/results', component: { template: '<div />' } },
        { path: '/student/results/:runId', component: ResultDetailView },
      ],
    })
    await router.push({ path: '/student/results/run-2', query: { projectId: 'project-2' } })
    await router.isReady()

    const wrapper = mount(ResultDetailView, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.get('h1').text()).toBe('量子隧穿实验'))
    expect(wrapper.text()).not.toContain('run-2')
    await wrapper.get('.technical__trigger').trigger('click')
    await vi.waitFor(() => expect(wrapper.get('.technical__list').text()).toContain('run-2'))
    expect(getOwnProjectEvaluationResult).toHaveBeenCalledWith({
      path: { projectId: 'project-2', runId: 'run-2' },
    })
    expect(wrapper.find('.back-link').attributes('href')).toContain('/student/results?projectId=project-2')
    wrapper.unmount()
  })

  async function mountResult(data: Record<string, unknown>) {
    vi.mocked(getOwnProjectEvaluationResult).mockResolvedValue({
      data: {
        runId: 'run-x', projectId: 'project-1', courseId: null, releaseId: 'release-1', frozenSubmissionId: 'submission-x', revision: 1,
        maxScore: 100, createdAt: '2026-09-08T11:00:00.000Z', updatedAt: '2026-09-08T12:00:00.000Z', completedAt: '2026-09-08T12:00:00.000Z',
        steps: [], ...data,
      } as never,
      error: undefined as never,
    })
    const router = createRouter({
      history: createWebHistory(),
      routes: [
        { path: '/student/results', component: { template: '<div />' } },
        { path: '/student/results/:runId', component: ResultDetailView },
      ],
    })
    await router.push({ path: '/student/results/run-x', query: { projectId: 'project-1' } })
    await router.isReady()
    const wrapper = mount(ResultDetailView, { global: { plugins: [router] } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.find('[data-testid="evaluation-outcome"]').exists()).toBe(true))
    return wrapper
  }

  it('titles the page with the experiment environment name instead of the run id', async () => {
    const wrapper = await mountResult({ state: 'succeeded', awardedScore: 70 })
    await vi.waitFor(() => expect(wrapper.get('h1').text()).toBe('量子隧穿实验'))
    expect(wrapper.text()).toContain('提交于')
    expect(document.title).toBe('量子隧穿实验 · 评测详情 · LabWeaver')
    wrapper.unmount()
  })

  it('shows a deterministic zero as a finished evaluation with the program reason', async () => {
    const wrapper = await mountResult({
      state: 'succeeded', awardedScore: 0,
      steps: [
        { position: 0, role: 'gate', state: 'failed', maxScore: 0, awardedScore: 0, diagnosticCode: 'LW_OJ_COMPILE_ERROR' },
        { position: 1, role: 'score', state: 'skipped', maxScore: 100, awardedScore: 0 },
      ],
    })
    const outcome = wrapper.get('[data-testid="evaluation-outcome"]')
    expect(outcome.text()).toContain('评测完成 · 0 分')
    expect(outcome.text()).toContain('程序编译失败')
    expect(outcome.get('.outcome-card__score').text()).toBe('0 / 100')
    expect(wrapper.findAll('.step-state').map((item) => item.text())).toEqual(['未通过', '已跳过'])
    expect(outcome.find('.diagnostic-banner').exists()).toBe(false)
    wrapper.unmount()
  })

  it('never shows a platform failure as zero points', async () => {
    const wrapper = await mountResult({
      state: 'failed', awardedScore: null, diagnosticCode: 'LW_OJ_INFRASTRUCTURE_ERROR',
      steps: [
        { position: 0, role: 'gate', state: 'succeeded', maxScore: 0, awardedScore: 0, diagnosticCode: 'LW_OJ_ACCEPTED' },
        { position: 1, role: 'score', state: 'failed', maxScore: 100, awardedScore: 0, diagnosticCode: 'LW_OJ_INFRASTRUCTURE_ERROR' },
      ],
    })
    const outcome = wrapper.get('[data-testid="evaluation-outcome"]')
    expect(outcome.text()).toContain('评测未完成 · 不计分')
    expect(outcome.text()).toContain('这不是 0 分')
    expect(outcome.text()).toContain('评测平台执行出错')
    expect(outcome.get('.outcome-card__score').text()).toBe('不计分')
    expect(wrapper.text()).not.toMatch(/\b0 \/ 100\b/)
    expect(wrapper.find('.step-card__score').exists()).toBe(false)
    expect(wrapper.findAll('.step-state').map((item) => item.text())).toEqual(['已完成', '未完成'])
    expect(outcome.get('.diagnostic-code').text()).toBe('LW_OJ_INFRASTRUCTURE_ERROR')
    expect(wrapper.find('[data-stage="result"]').classes()).toContain('submission-progress__stage--failed')
    wrapper.unmount()
  })

  it('reports a cancelled evaluation as not scored', async () => {
    const wrapper = await mountResult({ state: 'cancelled', awardedScore: null, diagnosticCode: 'LW_EVALUATION_CANCELLED' })
    expect(wrapper.get('[data-testid="evaluation-outcome"]').text()).toContain('已取消 · 不计分')
    expect(wrapper.text()).toContain('不会记为 0 分')
    wrapper.unmount()
  })

  it('does not invent the stage where an unfinished run stopped', async () => {
    const wrapper = await mountResult({ state: 'failed', awardedScore: null })
    const approval = wrapper.get('[data-stage="approval"]')
    expect(approval.classes()).toContain('submission-progress__stage--unknown')
    expect(approval.text()).toContain('平台未提供停在哪一阶段')
    wrapper.unmount()
  })

  it('shows a run stopped by an approval timeout at the approval stage with a resubmit hint', async () => {
    const wrapper = await mountResult({ state: 'failed', awardedScore: null, diagnosticCode: 'LW_TASK_RESOURCE_APPROVAL_TIMEOUT' })
    const outcome = wrapper.get('[data-testid="evaluation-outcome"]')
    expect(outcome.text()).toContain('评测未完成 · 不计分')
    expect(outcome.text()).toContain('评测所需资源的审批超时')
    expect(outcome.text()).toContain('重新提交')
    expect(wrapper.get('[data-stage="approval"]').classes()).toContain('submission-progress__stage--failed')
    expect(wrapper.get('[data-stage="running"]').classes()).toContain('submission-progress__stage--upcoming')
    wrapper.unmount()
  })

  it('shows the live stage of a run that has not ended and switches to the result once it ends', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    try {
      vi.mocked(getOwnProjectEvaluationResult).mockResolvedValue(notFinished)
      vi.mocked(getOwnProjectEvaluationTaskProgress).mockResolvedValue(runProgress('executing', {}, { resourceRequestId: 'request-1', resourceState: 'active' }))
      vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue(runProgress('executing'))
      const wrapper = await mountRun({ projectId: 'project-1', submissionId: 'submission-x' })
      await flushPromises()
      await vi.waitFor(() => expect(wrapper.find('[data-testid="evaluation-in-progress"]').exists()).toBe(true))
      expect(getOwnProjectEvaluationTaskProgress).toHaveBeenCalledWith({ path: { projectId: 'project-1', runId: 'run-x' } })
      expect(wrapper.text()).toContain('评测执行中')
      expect(wrapper.text()).toContain('评测结束前不显示分数')
      expect(wrapper.get('[aria-current="step"]').attributes('data-stage')).toBe('running')
      // The submission hint is used only after the server maps it to this run.
      await vi.waitFor(() => expect(wrapper.get('h1').text()).toBe('量子隧穿实验'))
      expect(document.title).toBe('量子隧穿实验 · 评测进行中 · LabWeaver')
      expect(wrapper.text()).not.toMatch(/%/)

      vi.mocked(getOwnProjectEvaluationTaskProgress).mockResolvedValue(runProgress('succeeded', { state: 'succeeded' }))
      vi.mocked(getOwnProjectEvaluationResult).mockResolvedValue({
        data: {
          runId: 'run-x', projectId: 'project-1', courseId: null, releaseId: 'release-1', frozenSubmissionId: 'submission-x', revision: 3,
          state: 'succeeded', awardedScore: 9, maxScore: 10, createdAt: '2026-09-08T11:00:00.000Z', updatedAt: '2026-09-08T12:00:00.000Z',
          completedAt: '2026-09-08T12:00:00.000Z', steps: [],
        } as never,
        error: undefined as never,
      })
      await vi.advanceTimersByTimeAsync(10_000)
      await flushPromises()
      await vi.waitFor(() => expect(wrapper.find('[data-testid="evaluation-outcome"]').exists()).toBe(true))
      expect(wrapper.get('.outcome-card__score').text()).toBe('9 / 10')
      const reads = vi.mocked(getOwnProjectEvaluationTaskProgress).mock.calls.length
      await vi.advanceTimersByTimeAsync(60_000)
      expect(vi.mocked(getOwnProjectEvaluationTaskProgress).mock.calls.length).toBe(reads)
      wrapper.unmount()
    } finally {
      vi.useRealTimers()
    }
  })

  it('ignores a submission hint that the server maps to a different run', async () => {
    vi.mocked(getOwnProjectEvaluationResult).mockResolvedValue(notFinished)
    vi.mocked(getOwnProjectEvaluationTaskProgress).mockResolvedValue(runProgress('queued', {}, { attempt: 0 }))
    vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue({ data: { runId: 'run-other' }, error: undefined } as never)
    const wrapper = await mountRun({ projectId: 'project-1', submissionId: 'submission-foreign' })
    await vi.waitFor(() => expect(wrapper.find('[data-testid="evaluation-in-progress"]').exists()).toBe(true))
    await flushPromises()
    expect(wrapper.get('h1').text()).toBe('实验提交')
    expect(getFrozenSubmission).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('explains a run this account cannot read and stops asking', async () => {
    vi.mocked(getOwnProjectEvaluationResult).mockResolvedValue(notFinished)
    vi.mocked(getOwnProjectEvaluationTaskProgress).mockResolvedValue(notFinished)
    const wrapper = await mountRun()
    await vi.waitFor(() => expect(wrapper.text()).toContain('找不到这次评测'))
    expect(wrapper.find('[data-testid="evaluation-in-progress"]').exists()).toBe(false)
    wrapper.unmount()
  })
})
