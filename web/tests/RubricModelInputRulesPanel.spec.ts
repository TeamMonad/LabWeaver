import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import RubricModelInputRulesPanel from '@/components/review/RubricModelInputRulesPanel.vue'
import {
  getActiveProjectLlmPolicy,
  getProjectAvailableActions,
  getProjectRubricModelInputRules,
  maintainProjectRubricModelInputRule,
} from '@/generated/contracts'
import type * as Contracts from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof Contracts>()
  return {
    ...actual,
    getActiveProjectLlmPolicy: vi.fn(),
    getProjectAvailableActions: vi.fn(),
    getProjectRubricModelInputRules: vi.fn(),
    maintainProjectRubricModelInputRule: vi.fn(),
  }
})

const rubricArtifact = { artifactId: 'artifact-rubric', mediaType: 'text/markdown', objectVersion: 'v7', sizeBytes: 512, storeBinding: 'materials' }

const problemPackage = {
  id: 'package-1',
  projectId: 'project-1',
  courseId: 'course-1',
  revision: 2,
  files: [
    { path: 'lab.md', sha256: 'a'.repeat(64), object: { ...rubricArtifact, artifactId: 'artifact-lab' } },
    { path: 'rubrics/review.md', sha256: 'b'.repeat(64), object: rubricArtifact },
  ],
  retention: { disposition: 'retain_until_revoked', retainUntil: null },
  completedAt: '2026-10-24T00:00:00.000Z',
}

const rubricIdentity = { artifact: rubricArtifact, packageId: 'package-1', packageRevision: 2, path: 'rubrics/review.md', sha256: 'b'.repeat(64) }

const policy = {
  id: 'policy-1',
  projectId: 'project-1',
  courseId: 'course-1',
  revision: 3,
  activatedAt: '2026-10-24T00:00:00.000Z',
  binding: { runtimeBinding: 'runtime-a', model: 'claude-sonnet', claudeCodeVersion: '2.1.0', workerImageSha256: 'c'.repeat(64), runtimeConfigSha256: 'd'.repeat(64), maxInFlightPerWorker: 1 },
  deniedDataClasses: [],
  budget: { maxInputTokens: 1, maxOutputTokens: 1, maxRequests: 1, maxCostMicrousd: 1, timeoutMilliseconds: 1000, maxTransientRetries: 0, maxSchemaRepairs: 0 },
  studentContentMode: 'manifest_allowlist_only',
}

function rulesView(rules: unknown[], policyRevision = 3) {
  return { projectId: 'project-1', courseId: 'course-1', policyId: `policy-${policyRevision}`, policyRevision, rules }
}

function availableActions(actions: string[]) {
  return {
    data: { projectId: 'project-1', actorId: 'teacher-1', membershipRevision: 1, validUntil: new Date(Date.now() + 3_600_000).toISOString(), actions },
    error: undefined,
  } as never
}

function policyWith(revision: number, model: string, id = `policy-${revision}`) {
  return { data: { ...policy, id, revision, binding: { ...policy.binding, model } } as never, error: undefined as never }
}

function confirmDialog(wrapper: VueWrapper) {
  return wrapper.findComponent({ name: 'ConfirmDialog' })
}

function rule(overrides: Record<string, unknown> = {}) {
  return {
    courseId: 'course-1',
    evaluationReleaseId: 'release-1',
    evaluationReleaseRevision: 5,
    rubric: rubricIdentity,
    visibility: 'course_public',
    approval: null,
    ...overrides,
  }
}

async function mountPanel(advisorySteps = [{ id: 'review', rubric: 'evaluator://rubrics/review.md' }]) {
  const wrapper = mount(RubricModelInputRulesPanel, {
    props: {
      projectId: 'project-1',
      evaluationReleaseId: 'release-1',
      evaluationReleaseRevision: 5,
      problemPackage: problemPackage as never,
      advisorySteps,
    },
  })
  await flushPromises()
  return wrapper
}

describe('RubricModelInputRulesPanel', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({ data: { ...policy, id: 'policy-3' } as never, error: undefined as never })
    vi.mocked(getProjectAvailableActions).mockResolvedValue(availableActions(['browse', 'edit']))
  })

  it('shows the rules but offers no change to a teacher who can only browse the project', async () => {
    vi.mocked(getProjectAvailableActions).mockResolvedValue(availableActions(['browse']))
    vi.mocked(getProjectRubricModelInputRules).mockResolvedValue({ data: rulesView([rule()]) as never, error: undefined as never })
    const wrapper = await mountPanel()

    expect(wrapper.get('[data-testid="rubric-rule-status"]').text()).toBe('课程内公开，尚未批准交给模型：不会交给模型。')
    expect(wrapper.find('.rubric-actions').exists()).toBe(false)
    expect(wrapper.get('[data-testid="rubric-edit-denied"]').text()).toContain('不包含“编辑”，不能修改评分规则的模型使用设置')
    expect(maintainProjectRubricModelInputRule).not.toHaveBeenCalled()
  })

  it('offers no change while the project permission cannot be read', async () => {
    vi.mocked(getProjectAvailableActions).mockResolvedValue({ data: undefined, error: { diagnosticCode: 'LW_UNAVAILABLE', detail: 'down', status: 503, retryable: true } } as never)
    vi.mocked(getProjectRubricModelInputRules).mockResolvedValue({ data: rulesView([]) as never, error: undefined as never })
    const wrapper = await mountPanel()

    expect(wrapper.find('[data-testid="rubric-set-public"]').exists()).toBe(false)
    expect(wrapper.get('[data-testid="rubric-edit-denied"]').text()).toContain('无法确认你在此项目的权限')
  })

  it('offers changes to a teacher with edit permission', async () => {
    vi.mocked(getProjectRubricModelInputRules).mockResolvedValue({ data: rulesView([rule()]) as never, error: undefined as never })
    const wrapper = await mountPanel()

    expect(wrapper.find('[data-testid="rubric-edit-denied"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="rubric-set-hidden"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="rubric-approve-model"]').exists()).toBe(true)
  })

  it('treats a rubric without a rule as hidden and makes it course-public with the exact identity', async () => {
    vi.mocked(getProjectRubricModelInputRules).mockResolvedValue({ data: rulesView([]) as never, error: undefined as never })
    vi.mocked(maintainProjectRubricModelInputRule).mockResolvedValue({ data: rulesView([rule()], 4) as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy)
      .mockResolvedValueOnce(policyWith(3, 'claude-sonnet'))
      .mockResolvedValueOnce(policyWith(4, 'claude-sonnet'))
    const wrapper = await mountPanel()

    expect(wrapper.get('[data-testid="rubric-rule-status"]').text()).toBe('隐藏：不会交给模型。')
    expect(wrapper.find('[data-testid="rubric-approve-model"]').exists()).toBe(false)
    await wrapper.get('[data-testid="rubric-set-public"]').trigger('click')
    await flushPromises()

    expect(maintainProjectRubricModelInputRule).toHaveBeenCalledWith({
      path: { projectId: 'project-1' },
      headers: { 'Idempotency-Key': expect.any(String), 'If-Match': '"rev-3"' },
      body: {
        action: { kind: 'set_visibility', visibility: 'course_public' },
        evaluationReleaseId: 'release-1',
        evaluationReleaseRevision: 5,
        rubric: rubricIdentity,
      },
    })
    expect(wrapper.get('[data-testid="rubric-rule-status"]').text()).toBe('课程内公开，尚未批准交给模型：不会交给模型。')
    // The save created policy revision 4; the AI settings are read again to stay on the same version.
    expect(getActiveProjectLlmPolicy).toHaveBeenCalledTimes(2)
    expect(wrapper.find('[data-testid="rubric-approve-model"]').attributes('disabled')).toBeUndefined()
  })

  it('approves model use separately with the current project model binding', async () => {
    vi.mocked(getProjectRubricModelInputRules).mockResolvedValue({ data: rulesView([rule()]) as never, error: undefined as never })
    const approved = rule({ approval: { approvedBy: 'teacher-1', approverCourseMembershipRevision: 1, model: 'claude-sonnet', runtimeBinding: 'runtime-a' } })
    vi.mocked(maintainProjectRubricModelInputRule).mockResolvedValue({ data: rulesView([approved], 4) as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy)
      .mockResolvedValueOnce(policyWith(3, 'claude-sonnet'))
      .mockResolvedValueOnce(policyWith(4, 'claude-sonnet'))
    const wrapper = await mountPanel()

    await wrapper.get('[data-testid="rubric-approve-model"]').trigger('click')
    await flushPromises()

    expect(vi.mocked(maintainProjectRubricModelInputRule).mock.calls[0][0].body.action).toEqual({ kind: 'approve_model_use', model: 'claude-sonnet', runtimeBinding: 'runtime-a' })
    expect(wrapper.get('[data-testid="rubric-rule-status"]').text()).toBe('课程内公开，已批准交给模型 claude-sonnet。')
    expect(wrapper.find('[data-testid="rubric-approve-model"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="rubric-revoke-model"]').exists()).toBe(true)
  })

  it('requires confirmation before revoking an approval, and does not count an approval for another model as permission', async () => {
    const staleApproval = rule({ approval: { approvedBy: 'teacher-1', approverCourseMembershipRevision: 1, model: 'claude-old', runtimeBinding: 'runtime-a' } })
    vi.mocked(getProjectRubricModelInputRules).mockResolvedValue({ data: rulesView([staleApproval]) as never, error: undefined as never })
    vi.mocked(maintainProjectRubricModelInputRule).mockResolvedValue({ data: rulesView([rule()], 4) as never, error: undefined as never })
    const wrapper = await mountPanel()

    expect(wrapper.get('[data-testid="rubric-rule-status"]').text()).toContain('与当前项目 AI 设置的模型 claude-sonnet 不同：不会交给模型')
    expect(wrapper.find('[data-testid="rubric-approve-model"]').exists()).toBe(true)

    await wrapper.get('[data-testid="rubric-revoke-model"]').trigger('click')
    await flushPromises()
    expect(confirmDialog(wrapper).props('open')).toBe(true)
    expect(confirmDialog(wrapper).props('description')).toContain('之后新开始的 AI 评语调用不会再读取这个评分规则')
    expect(maintainProjectRubricModelInputRule).not.toHaveBeenCalled()

    await confirmDialog(wrapper).vm.$emit('cancel')
    await flushPromises()
    expect(confirmDialog(wrapper).props('open')).toBe(false)
    expect(maintainProjectRubricModelInputRule).not.toHaveBeenCalled()

    await wrapper.get('[data-testid="rubric-revoke-model"]').trigger('click')
    await confirmDialog(wrapper).vm.$emit('confirm')
    await confirmDialog(wrapper).vm.$emit('confirm')
    await flushPromises()
    expect(maintainProjectRubricModelInputRule).toHaveBeenCalledTimes(1)
    expect(vi.mocked(maintainProjectRubricModelInputRule).mock.calls[0][0].body.action).toEqual({ kind: 'revoke_model_use' })
  })

  it('confirms hiding a rubric only when it would clear an existing approval', async () => {
    const approved = rule({ approval: { approvedBy: 'teacher-1', approverCourseMembershipRevision: 1, model: 'claude-sonnet', runtimeBinding: 'runtime-a' } })
    vi.mocked(getProjectRubricModelInputRules).mockResolvedValue({ data: rulesView([approved]) as never, error: undefined as never })
    vi.mocked(maintainProjectRubricModelInputRule).mockResolvedValue({ data: rulesView([rule({ visibility: 'hidden' })], 4) as never, error: undefined as never })
    const wrapper = await mountPanel()

    await wrapper.get('[data-testid="rubric-set-hidden"]').trigger('click')
    await flushPromises()
    expect(confirmDialog(wrapper).props('open')).toBe(true)
    expect(confirmDialog(wrapper).props('description')).toContain('同时清除已有的模型批准')
    expect(maintainProjectRubricModelInputRule).not.toHaveBeenCalled()

    await confirmDialog(wrapper).vm.$emit('confirm')
    await flushPromises()
    expect(maintainProjectRubricModelInputRule).toHaveBeenCalledTimes(1)
    expect(vi.mocked(maintainProjectRubricModelInputRule).mock.calls[0][0].body.action).toEqual({ kind: 'set_visibility', visibility: 'hidden' })
  })

  it('approves the model the project switched to after a conflict, not the one first read', async () => {
    vi.mocked(getProjectRubricModelInputRules)
      .mockResolvedValueOnce({ data: rulesView([rule()]) as never, error: undefined as never })
      .mockResolvedValueOnce({
        data: rulesView([rule({ approval: { approvedBy: 'teacher-2', approverCourseMembershipRevision: 1, model: 'model-a', runtimeBinding: 'runtime-a' } })], 4) as never,
        error: undefined as never,
      })
    vi.mocked(getActiveProjectLlmPolicy)
      .mockResolvedValueOnce(policyWith(3, 'model-a'))
      .mockResolvedValueOnce(policyWith(4, 'model-b'))
      .mockResolvedValueOnce(policyWith(5, 'model-b'))
    vi.mocked(maintainProjectRubricModelInputRule)
      .mockResolvedValueOnce({
        data: undefined as never,
        error: { type: 'about:blank', title: 'Conflict', status: 412, detail: 'stale', diagnosticCode: 'REVISION_CONFLICT', retryable: false, requestId: 'req-412' } as never,
      })
      .mockResolvedValueOnce({
        data: rulesView([rule({ approval: { approvedBy: 'teacher-1', approverCourseMembershipRevision: 1, model: 'model-b', runtimeBinding: 'runtime-a' } })], 5) as never,
        error: undefined as never,
      })
    const wrapper = await mountPanel()
    expect(wrapper.get('[data-testid="rubric-approve-model"]').text()).toBe('批准交给模型 model-a')

    await wrapper.get('[data-testid="rubric-approve-model"]').trigger('click')
    await flushPromises()
    // Rules and AI settings were both read again and agree on revision 4 with model B.
    expect(getProjectRubricModelInputRules).toHaveBeenCalledTimes(2)
    expect(getActiveProjectLlmPolicy).toHaveBeenCalledTimes(2)
    expect(wrapper.get('[data-testid="rubric-rule-status"]').text()).toBe('已批准的模型 model-a 与当前项目 AI 设置的模型 model-b 不同：不会交给模型。如需使用，请重新批准。')
    expect(wrapper.get('[data-testid="rubric-approve-model"]').text()).toBe('批准交给模型 model-b')

    await wrapper.get('[data-testid="rubric-approve-model"]').trigger('click')
    await flushPromises()
    const second = vi.mocked(maintainProjectRubricModelInputRule).mock.calls[1][0]
    expect(second.headers['If-Match']).toBe('"rev-4"')
    expect(second.body.action).toEqual({ kind: 'approve_model_use', model: 'model-b', runtimeBinding: 'runtime-a' })
    expect(wrapper.get('[data-testid="rubric-rule-status"]').text()).toBe('课程内公开，已批准交给模型 model-b。')
  })

  it('keeps approval closed while the AI settings and the rules belong to different policy versions', async () => {
    // `Promise.withResolvers` is outside the project's ES2020 lib, so the executor form captures the resolver.
    let resolvePolicy!: (value: unknown) => void
    vi.mocked(getProjectRubricModelInputRules).mockResolvedValue({ data: rulesView([rule()], 4) as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy)
      .mockResolvedValueOnce(policyWith(3, 'model-a'))
      .mockImplementationOnce(() => new Promise((resolve) => { resolvePolicy = resolve }) as never)
    const wrapper = await mountPanel()

    expect(getActiveProjectLlmPolicy).toHaveBeenCalledTimes(2)
    expect(wrapper.get('[data-testid="rubric-approve-model"]').attributes('disabled')).toBeDefined()
    expect(wrapper.find('[data-testid="rubric-policy-unsynced"]').exists()).toBe(true)
    await wrapper.get('[data-testid="rubric-approve-model"]').trigger('click')
    expect(maintainProjectRubricModelInputRule).not.toHaveBeenCalled()

    resolvePolicy(policyWith(4, 'model-b'))
    await flushPromises()
    expect(wrapper.get('[data-testid="rubric-approve-model"]').text()).toBe('批准交给模型 model-b')
    expect(wrapper.get('[data-testid="rubric-approve-model"]').attributes('disabled')).toBeUndefined()
  })

  it('re-reads the AI settings once and then keeps approval closed when the versions still disagree', async () => {
    vi.mocked(getProjectRubricModelInputRules).mockResolvedValue({ data: rulesView([rule()], 4) as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue(policyWith(3, 'model-a'))
    const wrapper = await mountPanel()
    await flushPromises()

    expect(getActiveProjectLlmPolicy).toHaveBeenCalledTimes(2)
    expect(wrapper.get('[data-testid="rubric-approve-model"]').attributes('disabled')).toBeDefined()
    expect(wrapper.get('[data-testid="rubric-rule-status"]').text()).toBe('课程内公开，尚未批准交给模型：不会交给模型。')
  })

  it('ignores a rule for another release revision or rubric version', async () => {
    vi.mocked(getProjectRubricModelInputRules).mockResolvedValue({
      data: rulesView([
        rule({ evaluationReleaseRevision: 4 }),
        rule({ rubric: { ...rubricIdentity, sha256: 'e'.repeat(64) } }),
      ]) as never,
      error: undefined as never,
    })
    const wrapper = await mountPanel()
    expect(wrapper.get('[data-testid="rubric-rule-status"]').text()).toBe('隐藏：不会交给模型。')
  })

  it('reloads the current rules after a revision conflict so the next change uses the new revision', async () => {
    vi.mocked(getProjectRubricModelInputRules)
      .mockResolvedValueOnce({ data: rulesView([]) as never, error: undefined as never })
      .mockResolvedValueOnce({ data: rulesView([rule()], 4) as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy)
      .mockResolvedValueOnce(policyWith(3, 'claude-sonnet'))
      .mockResolvedValueOnce(policyWith(4, 'claude-sonnet'))
      .mockResolvedValueOnce(policyWith(5, 'claude-sonnet'))
    vi.mocked(maintainProjectRubricModelInputRule).mockResolvedValueOnce({
      data: undefined as never,
      error: { type: 'about:blank', title: 'Conflict', status: 412, detail: 'stale', diagnosticCode: 'REVISION_CONFLICT', retryable: false, requestId: 'req-412' } as never,
    })
    const wrapper = await mountPanel()

    await wrapper.get('[data-testid="rubric-set-public"]').trigger('click')
    await flushPromises()
    expect(getProjectRubricModelInputRules).toHaveBeenCalledTimes(2)
    expect(wrapper.get('.diagnostic-banner .diagnostic-request-id').text()).toBe('req-412')
    expect(wrapper.get('[data-testid="rubric-rule-status"]').text()).toBe('课程内公开，尚未批准交给模型：不会交给模型。')

    vi.mocked(maintainProjectRubricModelInputRule).mockResolvedValueOnce({ data: rulesView([rule({ visibility: 'hidden' })], 5) as never, error: undefined as never })
    await wrapper.get('[data-testid="rubric-set-hidden"]').trigger('click')
    await flushPromises()
    const calls = vi.mocked(maintainProjectRubricModelInputRule).mock.calls
    expect(calls[1][0].headers['If-Match']).toBe('"rev-4"')
    expect(calls[1][0].headers['Idempotency-Key']).not.toBe(calls[0][0].headers['Idempotency-Key'])
  })

  it('blocks a rubric whose file is not in the approved package', async () => {
    vi.mocked(getProjectRubricModelInputRules).mockResolvedValue({ data: rulesView([]) as never, error: undefined as never })
    const wrapper = await mountPanel([{ id: 'review', rubric: 'evaluator://rubrics/missing.md' }])
    expect(wrapper.text()).toContain('材料包中没有这个路径的文件，无法设置')
    expect(wrapper.find('[data-testid="rubric-set-public"]').exists()).toBe(false)
  })

  it.each([
    ['LW_MODEL_INPUT_AUTHORIZATION_INVALID', 400, '评分规则的文件身份无效'],
    ['LW_MODEL_INPUT_AUTHORIZATION_DENIED', 403, '查看评分规则的模型使用设置需要是课程当前的授课教师'],
  ])('shows the Chinese reason before %s and its request id, and reuses the key on retry', async (code, status, reason) => {
    vi.mocked(getProjectRubricModelInputRules).mockResolvedValue({ data: rulesView([]) as never, error: undefined as never })
    vi.mocked(maintainProjectRubricModelInputRule).mockResolvedValue({
      data: undefined as never,
      error: { type: 'about:blank', title: 'Rejected', status, detail: 'rejected', diagnosticCode: code, retryable: false, requestId: `req-${status}` } as never,
    })
    const wrapper = await mountPanel()

    await wrapper.get('[data-testid="rubric-set-public"]').trigger('click')
    await flushPromises()
    const banner = wrapper.get('.diagnostic-banner')
    expect(banner.classes()).toContain('diagnostic-banner--error')
    expect(banner.get('.diagnostic-message').text()).toContain(reason)
    expect(banner.get('.diagnostic-code').text()).toBe(code)
    expect(banner.get('.diagnostic-request-id').text()).toBe(`req-${status}`)
    expect(banner.html().indexOf(reason)).toBeLessThan(banner.html().indexOf(code))

    await wrapper.get('[data-testid="rubric-set-public"]').trigger('click')
    await flushPromises()
    const keys = vi.mocked(maintainProjectRubricModelInputRule).mock.calls.map((call) => call[0].headers['Idempotency-Key'])
    expect(keys).toHaveLength(2)
    expect(keys[1]).toBe(keys[0])
  })
})
