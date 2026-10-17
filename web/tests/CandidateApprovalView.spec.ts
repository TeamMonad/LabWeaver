import { flushPromises, mount } from '@vue/test-utils'
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
  listProjectAgentRuns,
} from '@/generated/contracts'
import { agentRunHistory, environmentCandidateView, evaluationCandidateView, reviewPackage, reviewRun } from '../fixtures/authoring'

vi.mock('@/generated/contracts', () => ({
  listProjectAgentRuns: vi.fn(),
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

async function mountAt(query: Record<string, string> = { projectId: PROJECT }) {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/teacher/approvals', component: CandidateApprovalView },
      { path: '/teacher/materials', component: { template: '<div />' } },
    ],
  })
  await router.push({ path: '/teacher/approvals', query })
  await router.isReady()
  const wrapper = mount({ template: '<RouterView />' }, { global: { plugins: [router] }, attachTo: document.body })
  await flushPromises()
  await flushPromises()
  return { wrapper, router }
}

describe('CandidateApprovalView (审核与发布)', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    projectsState.selectedProjectId = PROJECT
    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok(agentRunHistory))
    vi.mocked(getProjectAgentRun).mockResolvedValue(ok(reviewRun))
    vi.mocked(getProjectEnvironmentCandidate).mockResolvedValue(ok(environmentCandidateView))
    vi.mocked(getProjectEvaluationCandidate).mockResolvedValue(ok(evaluationCandidateView))
    vi.mocked(getProjectProblemPackage).mockResolvedValue(ok(reviewPackage))
  })

  it('opens the pending version from the queue without asking for an internal ID', async () => {
    const { wrapper, router } = await mountAt()
    expect(wrapper.find('input[aria-label="AgentRun ID"]').exists()).toBe(false)
    expect(router.currentRoute.value.query.runId).toBe(reviewRun.id)
    const queue = wrapper.get('[data-testid="review-queue"]')
    // Work authoring runs are not reviewed here.
    expect(queue.find('[data-run-id="run-work-template"]').exists()).toBe(false)
    expect(queue.get(`[data-run-id="${reviewRun.id}"]`).attributes('aria-current')).toBe('page')
    // A failed run continues from the original task on the materials page.
    expect(queue.get('[data-run-id="run-authoring-failed"]').attributes('href')).toContain('/teacher/materials')
    expect(wrapper.get('#review-title').text()).toBe('光的干涉 · Python 数据分析实验')
  })

  it('shows readable environment permissions and scoring, and an honest residual admin-scope gap', async () => {
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id })
    const tabs = wrapper.findAll('[role="tab"]')
    expect(tabs.map((tab) => tab.text())).toEqual(['概览', '环境与权限', '评分规则', '材料', '学生预览', '技术详情'])
    await tabs[1].trigger('mousedown', { button: 0 })
    await flushPromises()
    const text = wrapper.text()
    expect(text).toContain('UID 65534')
    expect(text).toContain('特权模式')
    expect(text).toContain('只能访问平台允许的地址')
    expect(wrapper.get('[data-testid="permission-gap"]').text()).toContain('管理员策略允许的能力上限')

    await wrapper.findAll('[role="tab"]')[2].trigger('mousedown', { button: 0 })
    await flushPromises()
    expect(wrapper.text()).toContain('评分规则 · 满分 100 分')
    expect(wrapper.text()).toContain('测试组「条纹间距」40 分')
  })

  it('student preview never includes hidden tests, test groups or teacher materials', async () => {
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id })
    await wrapper.findAll('[role="tab"]')[4].trigger('mousedown', { button: 0 })
    await flushPromises()
    const preview = wrapper.get('[data-testid="student-preview"]').text()
    expect(preview).toContain('满分 100 分')
    expect(preview).toContain('评分项 1：最多 60 分')
    expect(preview).toContain('评分项 2：最多 40 分')
    expect(preview).toContain('analysis.ipynb')
    expect(preview).not.toMatch(/hidden\/tests|条纹间距|误差分析|fringe-spacing|handout|README/)
  })

  it('approves and publishes once: a repeated click reuses the same idempotency key', async () => {
    vi.mocked(completeProjectAuthoringApproval).mockResolvedValueOnce({ data: undefined, error: { message: 'Network Error' } } as never)
    const { wrapper } = await mountAt({ projectId: PROJECT, runId: reviewRun.id })
    const button = wrapper.get('[data-testid="approve-publish"]')
    expect(button.attributes('disabled')).toBeDefined()
    await wrapper.get('[data-testid="approve-card"] input[type="checkbox"]').setValue(true)
    await wrapper.get('[data-testid="approve-card"] textarea').setValue('已核对题面与评分标准')
    expect(button.attributes('disabled')).toBeUndefined()
    await button.trigger('click')
    await flushPromises()

    vi.mocked(completeProjectAuthoringApproval).mockResolvedValueOnce(ok({ id: 'approval-1', projectId: PROJECT, revision: 1 }))
    vi.mocked(getProjectAuthoringApproval).mockResolvedValue(ok({ approval: { id: 'approval-1', projectId: PROJECT, revision: 1 }, status: 'publishing' }))
    await wrapper.get('[data-testid="approve-publish"]').trigger('click')
    await flushPromises()

    const calls = vi.mocked(completeProjectAuthoringApproval).mock.calls
    expect(calls).toHaveLength(2)
    expect(calls[0][0].headers['Idempotency-Key']).toBe(calls[1][0].headers['Idempotency-Key'])
    expect(calls[1][0].body).toMatchObject({ packageId: reviewPackage.id, environmentCandidateId: environmentCandidateView.candidate.id, evaluationCandidateId: evaluationCandidateView.candidate.id })
    expect(wrapper.text()).toContain('已批准，正在发布')
  })

  it('explains the empty queue instead of asking for an ID', async () => {
    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok({ page: 1, pageSize: 50, hasMore: false, items: [] }))
    const { wrapper } = await mountAt()
    expect(wrapper.get('[data-testid="review-placeholder"]').text()).toContain('还没有可以审核的版本')
    expect(getProjectAgentRun).not.toHaveBeenCalled()
  })
})
