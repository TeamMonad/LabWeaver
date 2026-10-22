import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import CandidateApprovalView from '@/views/teacher/CandidateApprovalView.vue'
import {
  completeProjectAuthoringApproval,
  getProjectAgentRun,
  getProjectAuthoringApproval,
  getProjectEnvironmentCandidate,
  getProjectEvaluationCandidate,
  getProjectProblemPackage,
  listEnvironmentTemplateReleases,
  listProjectAgentRuns,
} from '@/generated/contracts'
import { agentRunHistory, environmentCandidateView, evaluationCandidateView, reviewPackage, reviewRun, reviewRunRelease } from '../fixtures/authoring'

vi.mock('@/generated/contracts', () => ({
  listProjectAgentRuns: vi.fn(),
  listEnvironmentTemplateReleases: vi.fn(),
  withdrawEnvironmentTemplateRelease: vi.fn(),
  getProjectAgentRun: vi.fn(),
  getProjectEnvironmentCandidate: vi.fn(),
  getProjectEvaluationCandidate: vi.fn(),
  getProjectProblemPackage: vi.fn(),
  getProjectAuthoringApproval: vi.fn(),
  completeProjectAuthoringApproval: vi.fn(),
  getProjectCandidateBuild: vi.fn(),
  cancelProjectCandidateBuild: vi.fn(),
}))

const PROJECT = reviewRun.projectId
const projectsState = reactive({
  projects: { kind: 'success' as const, data: [{ id: PROJECT, name: '课程项目', state: 'active' }] },
  selectedProjectId: PROJECT as string | null,
  select: vi.fn(),
})
vi.mock('@/composables/useProjects', () => ({ useProjects: () => projectsState }))

const ok = (data: unknown) => ({ data, error: undefined }) as never
const releases = (...items: unknown[]) => ok({ items, nextCursor: null })

/** Approval record bound to the fixture run's package and both candidates at their current revisions. */
function approvalRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: 'approval-1',
    projectId: PROJECT,
    revision: 1,
    packageId: reviewPackage.id,
    packageRevision: reviewPackage.revision,
    environmentCandidateId: environmentCandidateView.candidate.id,
    environmentCandidateRevision: environmentCandidateView.candidate.revision,
    evaluationCandidateId: evaluationCandidateView.candidate.id,
    evaluationCandidateRevision: evaluationCandidateView.candidate.revision,
    ...overrides,
  }
}

const RELEASE_APPROVAL_ID = reviewRunRelease.approval.id

/** Serves `getProjectAuthoringApproval` per approval id, as the server does. */
function serveApprovals(records: Record<string, unknown>) {
  vi.mocked(getProjectAuthoringApproval).mockImplementation(async (options) => {
    const record = records[options.path.approvalId]
    if (record === undefined) return { data: undefined, error: { diagnosticCode: 'LW_NOT_FOUND', detail: 'not found', status: 404 } } as never
    return ok(record)
  })
}

async function mountAt(query: Record<string, string> = { projectId: PROJECT }) {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/teacher/approvals', component: CandidateApprovalView },
      { path: '/teacher/materials', component: { template: '<div />' } },
      { path: '/teacher/labs', component: { template: '<div />' } },
    ],
  })
  await router.push({ path: '/teacher/approvals', query })
  await router.isReady()
  const wrapper = mount({ template: '<RouterView />' }, { global: { plugins: [router] }, attachTo: document.body })
  await flushPromises()
  await flushPromises()
  return { wrapper, router }
}

async function fillApprovalForm(wrapper: VueWrapper) {
  await wrapper.get('[data-testid="approve-card"] input[type="checkbox"]').setValue(true)
  await wrapper.get('[data-testid="approve-card"] textarea').setValue('已核对题面与评分标准')
}

describe('CandidateApprovalView (审核与发布)', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    projectsState.selectedProjectId = PROJECT
    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok(agentRunHistory))
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue(releases())
    vi.mocked(getProjectAgentRun).mockResolvedValue(ok(reviewRun))
    vi.mocked(getProjectEnvironmentCandidate).mockResolvedValue(ok(environmentCandidateView))
    vi.mocked(getProjectEvaluationCandidate).mockResolvedValue(ok(evaluationCandidateView))
    vi.mocked(getProjectProblemPackage).mockResolvedValue(ok(reviewPackage))
  })

  it('opens the pending version from the queue without asking for an internal ID', async () => {
    const { wrapper, router } = await mountAt()
    expect(wrapper.find('input[aria-label="AgentRun ID"]').exists()).toBe(false)
    expect(wrapper.find('select[aria-label="选择项目"]').exists()).toBe(false)
    expect(router.currentRoute.value.query.runId).toBe(reviewRun.id)
    const queue = wrapper.get('[data-testid="review-queue"]')
    // Work authoring runs are not reviewed here.
    expect(queue.find('[data-run-id="run-work-template"]').exists()).toBe(false)
    expect(queue.get(`[data-run-id="${reviewRun.id}"]`).attributes('aria-current')).toBe('page')
    // A failed run continues from the original task on the materials page.
    expect(queue.get('[data-run-id="run-authoring-failed"]').attributes('href')).toContain('/teacher/materials')
    expect(wrapper.get('#review-title').text()).toBe('光的干涉 · Python 数据分析实验')
  })

  it('shows readable environment permissions and scoring, and an honest gap for privileged mode', async () => {
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id })
    const tabs = wrapper.findAll('[role="tab"]')
    expect(tabs.map((tab) => tab.text())).toEqual(['概览', '环境与权限', '评分规则', '材料', '技术详情'])
    await tabs[1].trigger('mousedown', { button: 0 })
    await flushPromises()
    const text = wrapper.text()
    expect(text).toContain('必须以非 root 用户运行')
    expect(text).toContain('只能访问平台允许的地址')
    expect(wrapper.get('[data-testid="permission-gap"]').text()).toContain('还没有特权模式、capability 列表和管理员允许范围')

    await wrapper.findAll('[role="tab"]')[2].trigger('mousedown', { button: 0 })
    await flushPromises()
    expect(wrapper.text()).toContain('评分规则 · 满分 100 分')
    expect(wrapper.text()).toContain('测试组「条纹间距」40 分')
  })

  it('approves and publishes once: a repeated click reuses the same idempotency key', async () => {
    vi.mocked(completeProjectAuthoringApproval).mockResolvedValueOnce({ data: undefined, error: { message: 'Network Error' } } as never)
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id })
    const button = wrapper.get('[data-testid="approve-publish"]')
    expect(button.attributes('disabled')).toBeDefined()
    await fillApprovalForm(wrapper)
    expect(button.attributes('disabled')).toBeUndefined()
    await button.trigger('click')
    await flushPromises()

    vi.mocked(completeProjectAuthoringApproval).mockResolvedValueOnce(ok(approvalRecord()))
    vi.mocked(getProjectAuthoringApproval).mockResolvedValue(ok({ approval: approvalRecord(), status: 'publishing' }))
    await wrapper.get('[data-testid="approve-publish"]').trigger('click')
    await flushPromises()

    const calls = vi.mocked(completeProjectAuthoringApproval).mock.calls
    expect(calls).toHaveLength(2)
    expect(calls[0][0].headers['Idempotency-Key']).toBe(calls[1][0].headers['Idempotency-Key'])
    expect(calls[1][0].body).toMatchObject({ packageId: reviewPackage.id, environmentCandidateId: environmentCandidateView.candidate.id, evaluationCandidateId: evaluationCandidateView.candidate.id })
    expect(wrapper.text()).toContain('已批准')
    expect(wrapper.get('[data-testid="publication-status"]').attributes('data-status')).toBe('publishing')
    expect(wrapper.find('[data-testid="approve-publish"]').exists()).toBe(false)
  })

  it('lists the run under “已有发布版本” once publication is ready', async () => {
    vi.mocked(completeProjectAuthoringApproval).mockResolvedValueOnce(ok(approvalRecord()))
    serveApprovals({
      'approval-1': { approval: approvalRecord(), status: 'ready', environmentReleaseId: reviewRunRelease.id },
      [RELEASE_APPROVAL_ID]: { approval: approvalRecord({ id: RELEASE_APPROVAL_ID }), status: 'ready', environmentReleaseId: reviewRunRelease.id },
    })
    const { wrapper, router } = await mountAt({ projectId: PROJECT, runId: reviewRun.id })
    expect(listProjectAgentRuns).toHaveBeenCalledTimes(1)
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue(releases(reviewRunRelease))
    await fillApprovalForm(wrapper)
    await wrapper.get('[data-testid="approve-publish"]').trigger('click')
    await flushPromises()
    await flushPromises()

    expect(router.currentRoute.value.query.approvalId).toBe('approval-1')
    expect(wrapper.get('[data-testid="publication-status"]').attributes('data-status')).toBe('ready')
    expect(listProjectAgentRuns).toHaveBeenCalledTimes(2)
    const released = wrapper.get('[data-testid="review-queue"] [data-group="released"]')
    expect(released.find(`[data-run-id="${reviewRun.id}"]`).exists()).toBe(true)
  })

  it('does not offer a second approval when the release approval binds this exact version', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue(releases(reviewRunRelease))
    serveApprovals({ [RELEASE_APPROVAL_ID]: { approval: approvalRecord({ id: RELEASE_APPROVAL_ID }), status: 'ready' } })
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id })
    expect(getProjectAuthoringApproval).toHaveBeenCalledWith({ path: { projectId: PROJECT, approvalId: RELEASE_APPROVAL_ID } })
    expect(wrapper.get('[data-testid="review-published"]').text()).toContain('这一版已发布为版本 1')
    expect(wrapper.find('[data-testid="approve-publish"]').exists()).toBe(false)
    expect(completeProjectAuthoringApproval).not.toHaveBeenCalled()
    const queue = wrapper.get('[data-testid="review-queue"]')
    expect(queue.get('[data-group="released"]').text()).toContain('已发布版本 1')
    expect(queue.find('[data-group="review"]').exists()).toBe(false)
  })

  it('does not auto-open a run with a release, even one for an older candidate revision', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue(releases({ ...reviewRunRelease, candidateRevision: environmentCandidateView.candidate.revision - 1 }))
    const { wrapper, router } = await mountAt()
    expect(router.currentRoute.value.query.runId).toBeUndefined()
    expect(getProjectAgentRun).not.toHaveBeenCalled()
    expect(wrapper.get('[data-testid="review-placeholder"]').text()).toContain('从左侧选择')
    // The queue only knows the run has a release; it does not claim this version is published.
    const released = wrapper.get('[data-testid="review-queue"] [data-group="released"]')
    expect(released.find(`[data-run-id="${reviewRun.id}"]`).exists()).toBe(true)
    expect(released.find('h4').text()).toBe('已有发布版本，打开核对当前候选')
  })

  it('requires a new approval when the candidate changed after an earlier release', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue(releases({ ...reviewRunRelease, candidateRevision: environmentCandidateView.candidate.revision - 1 }))
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id })
    expect(wrapper.get('[data-testid="review-superseded"]').text()).toContain('之前发布过版本 1')
    expect(wrapper.find('[data-testid="review-published"]').exists()).toBe(false)
    await fillApprovalForm(wrapper)
    expect(wrapper.get('[data-testid="approve-publish"]').attributes('disabled')).toBeUndefined()
  })

  it('requires a new approval when only the Evaluation candidate changed after the release', async () => {
    // The Environment candidate and its revision are unchanged, so the release itself still matches it.
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue(releases(reviewRunRelease))
    serveApprovals({
      [RELEASE_APPROVAL_ID]: {
        approval: approvalRecord({ id: RELEASE_APPROVAL_ID, evaluationCandidateId: 'candidate-eval-before-retry', evaluationCandidateRevision: 1 }),
        status: 'ready',
      },
    })
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id })
    expect(wrapper.find('[data-testid="review-published"]').exists()).toBe(false)
    expect(wrapper.get('[data-testid="review-superseded"]').text()).toContain('之前发布过版本 1')
    await fillApprovalForm(wrapper)
    expect(wrapper.get('[data-testid="approve-publish"]').attributes('disabled')).toBeUndefined()
  })

  it('requires a new approval when only the package revision changed after the release', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue(releases(reviewRunRelease))
    serveApprovals({
      [RELEASE_APPROVAL_ID]: { approval: approvalRecord({ id: RELEASE_APPROVAL_ID, packageRevision: reviewPackage.revision - 1 }), status: 'ready' },
    })
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id })
    expect(wrapper.find('[data-testid="review-published"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="review-superseded"]').exists()).toBe(true)
  })

  it('fails closed when the approval behind a matching release cannot be read', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue(releases(reviewRunRelease))
    serveApprovals({})
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id })
    expect(wrapper.find('[data-testid="review-published"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="review-superseded"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="release-verify-error"]').exists()).toBe(true)
    await fillApprovalForm(wrapper)
    expect(wrapper.get('[data-testid="approve-publish"]').attributes('disabled')).toBeDefined()
    expect(wrapper.get('[data-testid="approve-blocked"]').text()).toContain('没能核对已发布版本的批准记录')

    serveApprovals({ [RELEASE_APPROVAL_ID]: { approval: approvalRecord({ id: RELEASE_APPROVAL_ID }), status: 'ready' } })
    await wrapper.get('[data-testid="release-verify-error"]').find('button').trigger('click')
    await flushPromises()
    expect(wrapper.get('[data-testid="review-published"]').text()).toContain('这一版已发布为版本 1')
  })

  it('fails closed when published versions cannot be read', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: undefined, error: { diagnosticCode: 'LW_ACCESS_DENIED', detail: 'denied', status: 403 } } as never)
    const { wrapper, router } = await mountAt()
    expect(wrapper.get('[data-testid="queue-release-error"]').text()).toContain('暂时不能批准')
    expect(router.currentRoute.value.query.runId).toBe(reviewRun.id)
    await fillApprovalForm(wrapper)
    expect(wrapper.get('[data-testid="approve-publish"]').attributes('disabled')).toBeDefined()
    expect(wrapper.get('[data-testid="approve-blocked"]').text()).toContain('请刷新左侧列表后再批准')
    await wrapper.get('[data-testid="approve-publish"]').trigger('click')
    await flushPromises()
    expect(completeProjectAuthoringApproval).not.toHaveBeenCalled()
  })

  it('ignores an approval id from the link that belongs to another run and keeps this run reviewable', async () => {
    vi.mocked(getProjectAuthoringApproval).mockResolvedValue(ok({
      approval: approvalRecord({ id: 'approval-other', environmentCandidateId: 'candidate-env-other', evaluationCandidateId: 'candidate-eval-other', packageId: 'package-other' }),
      status: 'ready',
    }))
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id, approvalId: 'approval-other' })
    expect(wrapper.get('[data-testid="approval-mismatch"]').text()).toContain('不属于当前版本')
    expect(wrapper.find('[data-testid="publication-status"]').exists()).toBe(false)
    await fillApprovalForm(wrapper)
    expect(wrapper.get('[data-testid="approve-publish"]').attributes('disabled')).toBeUndefined()
  })

  it('ignores an approval of an earlier candidate revision of the same run', async () => {
    vi.mocked(getProjectAuthoringApproval).mockResolvedValue(ok({
      approval: approvalRecord({ environmentCandidateRevision: environmentCandidateView.candidate.revision - 1 }),
      status: 'ready',
    }))
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id, approvalId: 'approval-1' })
    expect(wrapper.get('[data-testid="approval-mismatch"]').text()).toContain('不属于当前版本')
    expect(wrapper.find('[data-testid="publication-status"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="approve-publish"]').exists()).toBe(true)
  })

  it('shows the publication of a linked approval that binds this exact version', async () => {
    vi.mocked(getProjectAuthoringApproval).mockResolvedValue(ok({ approval: approvalRecord(), status: 'publishing' }))
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id, approvalId: 'approval-1' })
    expect(wrapper.find('[data-testid="approval-mismatch"]').exists()).toBe(false)
    expect(wrapper.get('[data-testid="publication-status"]').attributes('data-status')).toBe('publishing')
    expect(wrapper.find('[data-testid="approve-publish"]').exists()).toBe(false)
  })

  it('explains the empty queue instead of asking for an ID', async () => {
    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok({ page: 1, pageSize: 50, hasMore: false, items: [] }))
    const { wrapper } = await mountAt()
    expect(wrapper.get('[data-testid="review-placeholder"]').text()).toContain('还没有可以审核的版本')
    expect(getProjectAgentRun).not.toHaveBeenCalled()
  })
})
