import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { defineComponent, onMounted } from 'vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import PolicyListView from '@/views/admin/PolicyListView.vue'
import { useProjects } from '@/composables/useProjects'
import {
  createProjectLlmPolicy,
  getActiveProjectLlmPolicy,
  getProjectLlmPolicyOptions,
  listProjects,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    createProjectLlmPolicy: vi.fn(),
    getActiveProjectLlmPolicy: vi.fn(),
    getProjectLlmPolicyOptions: vi.fn(),
    listProjects: vi.fn(),
  }
})

const project = {
  id: 'project-1',
  name: 'Demo project',
  description: null,
  ownerActorId: 'actor-1',
  courseId: null,
  revision: 1,
  state: 'active',
  createdAt: '2026-07-11T00:00:00.000Z',
  updatedAt: '2026-07-11T00:00:00.000Z',
}

const options = {
  models: [{ model: 'approved-model-v1', label: 'Approved model' }],
  defaultModel: 'approved-model-v1',
  runtimeBinding: 'claude-code-production',
  claudeCodeVersion: '2.1.207',
  maxInFlightPerWorker: 2,
}

const policy = {
  id: '0190f0c0-0000-7000-8000-000000000001',
  projectId: 'project-1',
  courseId: null,
  revision: 2,
  activatedAt: '2026-07-11T00:00:00.000Z',
  binding: { runtimeBinding: 'claude-code-production', model: 'approved-model-v1', claudeCodeVersion: '2.1.207', maxInFlightPerWorker: 2 },
  deniedDataClasses: ['secret', 'token', 'private_key', 'personally_identifiable_information', 'unallowlisted_student_submission'],
  budget: { maxInputTokens: 100_000, maxOutputTokens: 16_000, maxRequests: 8, maxCostMicrousd: 2_000_000, timeoutMilliseconds: 120_000, maxTransientRetries: 2, maxSchemaRepairs: 2 },
  studentContentMode: 'manifest_allowlist_only',
}

describe('PolicyListView', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    vi.mocked(listProjects).mockResolvedValue({ data: [project] as never, error: undefined as never })
    vi.mocked(getProjectLlmPolicyOptions).mockResolvedValue({ data: options as never, error: undefined as never })
  })

  async function mountView(path = '/researcher/ai-policy?projectId=project-1') {
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [
        { path: '/researcher/ai-policy', component: PolicyListView },
        { path: '/admin/policies', component: PolicyListView },
      ],
    })
    await router.push(path)
    await router.isReady()
    const wrapper = mount(PolicyListView, { global: { plugins: [router] } })
    return { router, wrapper }
  }

  async function preloadProjectContext() {
    let resolveLoaded!: () => void
    let projectStore!: ReturnType<typeof useProjects>
    const loaded = new Promise<void>((resolve) => { resolveLoaded = resolve })
    const loader = mount(defineComponent({
      setup() {
        projectStore = useProjects()
        onMounted(async () => {
          await projectStore.load()
          resolveLoaded()
        })
        return () => null
      },
    }))
    await loaded
    loader.unmount()
    return projectStore
  }

  it('explains the missing policy and saves a first policy with an idempotency key', async () => {
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({
      data: undefined as never,
      error: { status: 404, diagnosticCode: 'LW_POLICY_NOT_FOUND', detail: 'no active policy' } as never,
    })
    vi.mocked(createProjectLlmPolicy).mockResolvedValue({ data: policy as never, error: undefined as never })

    const { wrapper } = await mountView()
    await vi.waitFor(() => expect(wrapper.find('[data-testid="policy-empty-state"]').exists()).toBe(true))
    expect(wrapper.text()).toContain('尚未配置')

    await wrapper.find('[data-testid="policy-material-consent"]').setValue(true)
    await wrapper.find('[data-testid="policy-form"]').trigger('submit')
    await vi.waitFor(() => expect(createProjectLlmPolicy).toHaveBeenCalledTimes(1))
    const request = vi.mocked(createProjectLlmPolicy).mock.calls[0][0]
    expect(request.path).toEqual({ projectId: 'project-1' })
    expect(request.headers['Idempotency-Key']).toBeTruthy()
    expect(request.headers['If-Match']).toBeUndefined()
    expect(request.body.binding.model).toBe('approved-model-v1')
    expect(request.body.budget.maxCostMicrousd).toBe(2_000_000)
    expect(request.body.budget.timeoutMilliseconds).toBe(120_000)
  })

  it('uses the active revision when editing and gives guidance for denied access', async () => {
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({ data: policy as never, error: undefined as never })
    vi.mocked(createProjectLlmPolicy).mockResolvedValue({ data: { ...policy, revision: 3 } as never, error: undefined as never })
    const { wrapper } = await mountView()
    await vi.waitFor(() => expect(wrapper.find('[data-testid="policy-form"]').exists()).toBe(true))
    await wrapper.find('[data-testid="policy-form"]').trigger('submit')
    await vi.waitFor(() => expect(createProjectLlmPolicy).toHaveBeenCalledTimes(1))
    expect(vi.mocked(createProjectLlmPolicy).mock.calls[0][0].headers['If-Match']).toBe('"rev-2"')

    vi.mocked(getProjectLlmPolicyOptions).mockResolvedValue({
      data: undefined as never,
      error: { diagnosticCode: 'LW_AUTH_SCOPE_DENIED', detail: 'denied', retryable: false } as never,
    })
    await wrapper.find('button[aria-label="刷新项目 AI 设置"]').trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('当前账号没有修改这个项目 AI 设置的权限'))
  })

  it('loads the selected private work on the admin route and keeps its policy scope denied', async () => {
    const privateWorkProject = { ...project, id: 'private-work', name: 'Private Work' }
    vi.mocked(listProjects).mockResolvedValue({ data: [privateWorkProject] as never, error: undefined as never })
    vi.mocked(getProjectLlmPolicyOptions).mockResolvedValue({
      data: undefined as never,
      error: { diagnosticCode: 'LW_AUTH_SCOPE_DENIED', detail: 'denied', retryable: false } as never,
    })

    const projectStore = await preloadProjectContext()
    expect(projectStore.projects).toMatchObject({ kind: 'success', data: [privateWorkProject] })
    expect(projectStore.selectedProjectId).toBe('private-work')

    const { wrapper } = await mountView('/admin/policies')
    await vi.waitFor(() => expect(getProjectLlmPolicyOptions).toHaveBeenCalledWith({ path: { projectId: 'private-work' } }))

    expect(wrapper.text()).toContain('当前账号没有修改这个项目 AI 设置的权限')
    expect(wrapper.find('[data-testid="policy-form"]').exists()).toBe(false)
    expect(wrapper.text()).not.toContain('选择一个项目')
  })
})
