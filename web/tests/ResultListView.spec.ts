import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createWebHistory } from 'vue-router'
import ResultListView from '@/views/student/ResultListView.vue'
import {
  getEnvironment,
  getFrozenSubmission,
  getOwnProjectEvaluationTaskForSubmission,
  listOwnProjectEvaluationResults,
  listProjects,
} from '@/generated/contracts'
import { useAuth } from '@/composables/useAuth'
import { rememberSubmission } from '@/utils/trackedSubmissions'

function signIn(actorId: string | null) {
  useAuth().user.value = actorId ? { expired: false, profile: { actor_id: actorId, roles: ['student'] } } : null
}

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    listOwnProjectEvaluationResults: vi.fn(),
    getFrozenSubmission: vi.fn(),
    getEnvironment: vi.fn(),
    getOwnProjectEvaluationTaskForSubmission: vi.fn(),
    listProjects: vi.fn(),
  }
})

function taskProgress(phase: string, extra: Record<string, unknown> = {}, attempt: Record<string, unknown> = {}) {
  return {
    data: {
      runId: 'run-new', projectId: 'project-b', revision: 2, state: 'running',
      current: [{ stepRunId: 'step-1', progress: { attempt: 1, phase, nextSteps: ['getOwnProjectEvaluationTaskProgress'], ...attempt } }],
      history: [], page: 1, pageSize: 20, hasMore: false, ...extra,
    },
    error: undefined,
  } as never
}

const projectA = {
  id: 'project-a',
  name: 'Project A',
  description: 'Selected project',
  ownerActorId: 'student-1',
  courseId: null,
  revision: 1,
  state: 'active' as const,
  createdAt: '2026-09-08T00:00:00.000Z',
  updatedAt: '2026-09-08T00:00:00.000Z',
}

const projectB = {
  ...projectA,
  id: 'project-b',
  name: 'Project B',
}

const resultB = {
  runId: 'run-b',
  projectId: 'project-b',
  courseId: null,
  releaseId: 'release-b',
  frozenSubmissionId: 'submission-b',
  revision: 1,
  state: 'succeeded' as const,
  awardedScore: 9,
  maxScore: 10,
  createdAt: '2026-09-08T11:00:00.000Z',
  updatedAt: '2026-09-08T12:00:00.000Z',
  completedAt: '2026-09-08T12:00:00.000Z',
  steps: [],
}

const mountedWrappers: Array<{ unmount: () => void }> = []

async function mountAt(query: Record<string, string>) {
  const router = createRouter({
    history: createWebHistory(),
    routes: [
      { path: '/student/results', component: ResultListView },
      { path: '/student/results/:runId', component: { template: '<div />' } },
    ],
  })
  await router.push({ path: '/student/results', query })
  await router.isReady()
  const wrapper = mount(ResultListView, { global: { plugins: [router] } })
  mountedWrappers.push(wrapper)
  return { wrapper, router }
}

describe('ResultListView', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.clearAllMocks()
    vi.mocked(listProjects).mockResolvedValue({
      data: [projectA, projectB] as never,
      error: undefined as never,
    })
    vi.mocked(listOwnProjectEvaluationResults).mockResolvedValue({
      data: { items: [resultB], nextCursor: null } as never,
      error: undefined as never,
    })
    vi.mocked(getFrozenSubmission).mockImplementation(async (options) => {
      if (options.path.submissionId === 'submission-foreign') {
        return { data: undefined, error: { status: 404, diagnosticCode: 'LW_COLLECT_SUBMISSION_NOT_FOUND', detail: 'missing' } } as never
      }
      return {
        data: {
          id: options.path.submissionId,
          projectId: options.path.projectId,
          actorId: 'student-1',
          frozenAt: '2026-09-08T10:30:00.000Z',
          environment: { environmentId: 'env-1', environmentRevision: 1, releaseId: 'tpl-1', releaseVersion: 1, runtimeKind: 'container' },
        },
        error: undefined,
      } as never
    })
    vi.mocked(getEnvironment).mockResolvedValue({ data: { id: 'env-1', displayLabel: '量子隧穿实验' }, error: undefined } as never)
    vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue(taskProgress('waiting_resource_approval', {}, { resourceRequestId: 'request-1', resourceState: 'reviewing' }))
    localStorage.clear()
    signIn('student-1')
  })

  afterEach(() => {
    mountedWrappers.splice(0).forEach((wrapper) => wrapper.unmount())
  })

  it('uses a direct project route context and carries it into the detail link', async () => {
    const { wrapper } = await mountAt({ projectId: 'project-b' })

    await vi.waitFor(() => expect(wrapper.find('.result-link').text()).toBe('量子隧穿实验'))
    expect(wrapper.text()).not.toContain('run-b')
    expect(listOwnProjectEvaluationResults).toHaveBeenCalledWith({
      path: { projectId: 'project-b' },
      query: { cursor: undefined, limit: 50 },
    })
    expect(wrapper.find('.result-link').attributes('href')).toContain('/student/results/run-b?projectId=project-b')
  })

  function result(runId: string, submissionId: string, extra: Record<string, unknown>) {
    return { ...resultB, runId, frozenSubmissionId: submissionId, ...extra }
  }

  it('separates a deterministic zero from a platform failure and a cancellation', async () => {
    vi.mocked(listOwnProjectEvaluationResults).mockResolvedValue({
      data: {
        items: [
          result('run-zero', 'submission-zero', { awardedScore: 0 }),
          result('run-platform', 'submission-platform', { state: 'failed', awardedScore: null, diagnosticCode: 'LW_OJ_INFRASTRUCTURE_ERROR' }),
          result('run-cancelled', 'submission-cancelled', { state: 'cancelled', awardedScore: null }),
        ],
        nextCursor: null,
      } as never,
      error: undefined as never,
    })
    const { wrapper } = await mountAt({ projectId: 'project-b' })
    await vi.waitFor(() => expect(wrapper.findAll('[data-run-id]')).toHaveLength(3))
    const zero = wrapper.get('[data-run-id="run-zero"]')
    expect(zero.text()).toContain('评测完成 · 0 分')
    expect(zero.get('.result-score').text()).toBe('0 / 10')
    const platform = wrapper.get('[data-run-id="run-platform"]')
    expect(platform.text()).toContain('评测未完成 · 不计分')
    expect(platform.text()).toContain('这不是 0 分')
    expect(platform.text()).toContain('评测平台执行出错')
    expect(platform.find('.result-score').exists()).toBe(false)
    expect(wrapper.get('[data-run-id="run-cancelled"]').text()).toContain('已取消 · 不计分')
  })

  it('keeps a submission made earlier visible with its server stage, then shows its result without a manual refresh', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    try {
      rememberSubmission('student-1', 'project-b', 'submission-new')
      vi.mocked(listOwnProjectEvaluationResults).mockResolvedValue({ data: { items: [resultB], nextCursor: null } as never, error: undefined as never })
      const { wrapper } = await mountAt({ projectId: 'project-b', submissionId: 'submission-new' })
      await flushPromises()
      await vi.waitFor(() => expect(wrapper.find('[data-testid="pending-submissions"]').exists()).toBe(true))
      const card = () => wrapper.get('[data-submission-id="submission-new"]')
      expect(card().classes()).toContain('result-card--highlight')
      await vi.waitFor(() => expect(card().text()).toContain('等待资源审批'))
      expect(getOwnProjectEvaluationTaskForSubmission).toHaveBeenCalledWith({ path: { projectId: 'project-b', submissionId: 'submission-new' } })
      expect(card().text()).toContain('量子隧穿实验')
      expect(card().get('[data-stage="submitted"]').classes()).toContain('submission-progress__stage--done')
      expect(card().get('[data-stage="queued"]').classes()).toContain('submission-progress__stage--done')
      expect(card().get('[aria-current="step"]').attributes('data-stage')).toBe('approval')
      expect(card().get('.result-link').attributes('href')).toContain('/student/results/run-new?projectId=project-b&submissionId=submission-new')
      expect(card().text()).not.toMatch(/%/)

      vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue(taskProgress('executing', {}, { resourceRequestId: 'request-1', resourceState: 'active' }))
      await vi.advanceTimersByTimeAsync(10_000)
      await flushPromises()
      await vi.waitFor(() => expect(card().get('[aria-current="step"]').attributes('data-stage')).toBe('running'))

      vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue(taskProgress('succeeded', { state: 'succeeded' }))
      vi.mocked(listOwnProjectEvaluationResults).mockResolvedValue({
        data: { items: [result('run-new', 'submission-new', { awardedScore: 7 }), resultB], nextCursor: null } as never,
        error: undefined as never,
      })
      await vi.advanceTimersByTimeAsync(15_000)
      await flushPromises()
      await vi.waitFor(() => expect(wrapper.find('[data-testid="pending-submissions"]').exists()).toBe(false))
      expect(wrapper.get('[data-run-id="run-new"]').classes()).toContain('result-card--highlight')
      // The finished submission is no longer tracked locally; the server list is the record.
      expect(localStorage.getItem('labweaver_submissions:student-1')).toBeNull()
    } finally {
      vi.useRealTimers()
    }
  })

  it('shows a failed earlier attempt only as history while the retry is running', async () => {
    rememberSubmission('student-1', 'project-b', 'submission-new')
    vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue(taskProgress(
      'executing',
      { history: [{ stepRunId: 'step-1', progress: { attempt: 1, phase: 'failed', diagnosticCode: 'LW_OJ_JOB_FAILED', nextSteps: [] } }] },
      { attempt: 2, resourceRequestId: 'request-2', resourceState: 'active' },
    ))
    const { wrapper } = await mountAt({ projectId: 'project-b' })
    await vi.waitFor(() => expect(wrapper.find('[data-submission-id="submission-new"]').text()).toContain('评测执行中'))
    const card = wrapper.get('[data-submission-id="submission-new"]')
    expect(card.get('[aria-current="step"]').attributes('data-stage')).toBe('running')
    expect(card.text()).toContain('第 2 次尝试')
    expect(card.text()).toContain('此前有 1 次尝试没有完成')
    expect(card.text()).not.toContain('评测任务没有正常结束')
    expect(card.findAll('.submission-progress__stage--failed')).toHaveLength(0)
  })

  it('keeps the last confirmed stage when a progress read fails, with the reason before the code', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    try {
      rememberSubmission('student-1', 'project-b', 'submission-new')
      const { wrapper } = await mountAt({ projectId: 'project-b' })
      await flushPromises()
      await vi.waitFor(() => expect(wrapper.find('[data-submission-id="submission-new"]').text()).toContain('等待资源审批'))
      vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue({
        data: undefined,
        error: { status: 503, diagnosticCode: 'LW_TASK_PROGRESS_RESOURCE_UNAVAILABLE', detail: 'unavailable', retryable: true },
      } as never)
      await vi.advanceTimersByTimeAsync(10_000)
      await flushPromises()
      const card = wrapper.get('[data-submission-id="submission-new"]')
      await vi.waitFor(() => expect(card.find('[data-testid="task-read-error"]').exists()).toBe(true))
      expect(card.get('[aria-current="step"]').attributes('data-stage')).toBe('approval')
      const error = card.get('[data-testid="task-read-error"]').text()
      expect(error.indexOf('资源服务暂时不可用')).toBeLessThan(error.indexOf('LW_TASK_PROGRESS_RESOURCE_UNAVAILABLE'))
    } finally {
      vi.useRealTimers()
    }
  })

  it('shows a submission whose evaluation the server has not created yet as waiting', async () => {
    rememberSubmission('student-1', 'project-b', 'submission-new')
    vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue({
      data: undefined,
      error: { status: 404, diagnosticCode: 'LW_EVALUATION_RUN_NOT_FOUND', detail: 'missing' },
    } as never)
    const { wrapper } = await mountAt({ projectId: 'project-b' })
    await vi.waitFor(() => expect(wrapper.find('[data-submission-id="submission-new"]').text()).toContain('等待创建评测'))
    const card = wrapper.get('[data-submission-id="submission-new"]')
    expect(card.get('[aria-current="step"]').attributes('data-stage')).toBe('queued')
    expect(card.find('.result-link').exists()).toBe(false)
  })

  it('stops polling while the tab is hidden', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    const visibility = vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible')
    try {
      rememberSubmission('student-1', 'project-b', 'submission-new')
      await mountAt({ projectId: 'project-b' })
      await flushPromises()
      const calls = vi.mocked(listOwnProjectEvaluationResults).mock.calls.length
      visibility.mockReturnValue('hidden')
      document.dispatchEvent(new Event('visibilitychange'))
      await vi.advanceTimersByTimeAsync(60_000)
      expect(vi.mocked(listOwnProjectEvaluationResults).mock.calls.length).toBe(calls)
      visibility.mockReturnValue('visible')
      document.dispatchEvent(new Event('visibilitychange'))
      await flushPromises()
      expect(vi.mocked(listOwnProjectEvaluationResults).mock.calls.length).toBe(calls + 1)
    } finally {
      visibility.mockRestore()
      vi.useRealTimers()
    }
  })

  it('drops a remembered submission the server no longer returns to this account', async () => {
    rememberSubmission('student-1', 'project-b', 'submission-foreign')
    const { wrapper } = await mountAt({ projectId: 'project-b' })
    await flushPromises()
    await vi.waitFor(() => expect(localStorage.getItem('labweaver_submissions:student-1')).toBeNull())
    expect(wrapper.find('[data-testid="pending-submissions"]').exists()).toBe(false)
  })

  it('lists a remembered submission only after the server returns it to this student', async () => {
    rememberSubmission('student-1', 'project-b', 'submission-new')
    vi.mocked(getFrozenSubmission).mockResolvedValue({ data: undefined, error: { status: 503, diagnosticCode: 'LW_X', detail: 'down', retryable: true } } as never)
    const { wrapper } = await mountAt({ projectId: 'project-b' })
    await flushPromises()
    await vi.waitFor(() => expect(getFrozenSubmission).toHaveBeenCalled())
    await flushPromises()
    expect(wrapper.find('[data-testid="pending-submissions"]').exists()).toBe(false)
    expect(getOwnProjectEvaluationTaskForSubmission).not.toHaveBeenCalled()
  })

  it('never lists a submission the server attributes to another student', async () => {
    rememberSubmission('student-1', 'project-b', 'submission-new')
    vi.mocked(getFrozenSubmission).mockResolvedValue({
      data: {
        id: 'submission-new', projectId: 'project-b', actorId: 'student-2', frozenAt: '2026-09-08T10:30:00.000Z',
        environment: { environmentId: 'env-1', environmentRevision: 1, releaseId: 'tpl-1', releaseVersion: 1, runtimeKind: 'container' },
      },
      error: undefined,
    } as never)
    const { wrapper } = await mountAt({ projectId: 'project-b' })
    await vi.waitFor(() => expect(localStorage.getItem('labweaver_submissions:student-1')).toBeNull())
    expect(wrapper.find('[data-testid="pending-submissions"]').exists()).toBe(false)
  })

  it('does not show the previous student\'s submissions after another student signs in on the same browser', async () => {
    rememberSubmission('student-1', 'project-b', 'submission-new')
    signIn('student-2')
    expect(localStorage.getItem('labweaver_submissions:student-1')).toBeNull()
    const { wrapper } = await mountAt({ projectId: 'project-b' })
    await flushPromises()
    expect(wrapper.find('[data-testid="pending-submissions"]').exists()).toBe(false)
    expect(getFrozenSubmission).not.toHaveBeenCalledWith({ path: { projectId: 'project-b', submissionId: 'submission-new' } })
    expect(getOwnProjectEvaluationTaskForSubmission).not.toHaveBeenCalled()
  })

  it('discards the tracked submissions when the student signs out', async () => {
    rememberSubmission('student-1', 'project-b', 'submission-new')
    signIn(null)
    expect(localStorage.getItem('labweaver_submissions:student-1')).toBeNull()
    signIn('student-1')
    const { wrapper } = await mountAt({ projectId: 'project-b' })
    await flushPromises()
    expect(wrapper.find('[data-testid="pending-submissions"]').exists()).toBe(false)
  })

  it('clears a waiting submission\'s progress when the server answers for another project', async () => {
    rememberSubmission('student-1', 'project-b', 'submission-new')
    vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValue(taskProgress('executing', { projectId: 'project-a' }))
    const { wrapper } = await mountAt({ projectId: 'project-b' })
    await vi.waitFor(() => expect(wrapper.find('[data-testid="task-read-error"]').exists()).toBe(true))
    const card = wrapper.get('[data-submission-id="submission-new"]')
    expect(card.get('[data-testid="task-read-error"]').text()).toContain('EVALUATION_TASK_STALE_CONTEXT')
    expect(card.text()).not.toContain('评测执行中')
    expect(card.find('.result-link').exists()).toBe(false)
  })
})
