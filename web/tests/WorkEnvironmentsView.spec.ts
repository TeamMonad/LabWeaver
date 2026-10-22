import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import WorkEnvironmentsView from '@/views/researcher/WorkEnvironmentsView.vue'
import {
  getActiveProjectLlmPolicy,
  listEnvironments,
  listEnvironmentTemplateReleases,
  listProjectResourceLeases,
  listProjectResourceRequests,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', () => ({
  getActiveProjectLlmPolicy: vi.fn(),
  listEnvironments: vi.fn(),
  listEnvironmentTemplateReleases: vi.fn(),
  listProjectResourceLeases: vi.fn(),
  listProjectResourceRequests: vi.fn(),
  withdrawEnvironmentTemplateRelease: vi.fn(),
}))

vi.mock('@/views/student/EnvironmentEntryView.vue', () => ({
  default: { name: 'EnvironmentEntryView', template: '<div data-testid="environment-detail" />' },
}))

const project = {
  id: 'project-1',
  name: '课程项目',
  description: '',
  ownerActorId: 'owner-1',
  courseId: null,
  state: 'active',
  revision: 1,
  createdAt: '2026-09-01T00:00:00.000Z',
  updatedAt: '2026-09-01T00:00:00.000Z',
}

const projectsState = reactive({
  projects: { kind: 'success' as const, data: [project] },
  selectedProjectId: project.id as string | null,
  select: vi.fn(),
})

vi.mock('@/composables/useProjects', () => ({ useProjects: () => projectsState }))

const containerRelease = {
  id: 'release-container',
  version: 3,
  projectId: 'project-1',
  courseId: null,
  runtimeKind: 'container',
  publishedAt: '2026-09-12T10:30:00.000Z',
  publishedBy: 'teacher-1',
  agentRunId: 'run-1',
  candidateId: 'candidate-1',
  candidateRevision: 1,
  approval: {},
  artifact: { kind: 'container', id: 'artifact-1', build_request_id: 'build-1', repository: 'registry.example/labs/python', digest: 'sha256:0123456789abcdef0123' },
}
const vmRelease = {
  ...containerRelease,
  id: 'release-vm',
  version: 1,
  runtimeKind: 'virtual_machine',
  artifact: { kind: 'virtual_machine', id: 'artifact-2', format: 'qcow2', base_disk: { binding: 'disk', capacityBytes: 40 * 1024 ** 3, sourceRegistryDigest: 'sha256:x' } },
}
const withdrawnRelease = { ...containerRelease, id: 'release-old', version: 1, withdrawal: { reasonCode: 'TEACHER_WITHDRAWN' } }

const workSummary = {
  id: 'env-work-1',
  projectId: 'project-1',
  class: 'work',
  displayLabel: '分析工作空间',
  runtimeKind: 'container',
  observedState: 'ready',
  desiredState: 'running',
  releaseId: 'release-container',
  releaseVersion: 2,
  updatedAt: '2026-09-14T08:00:00.000Z',
}

function problem(status: number, diagnosticCode: string) {
  return { data: undefined, error: { status, diagnosticCode, detail: diagnosticCode, requestId: 'req-1', retryable: false, title: 'x', type: 'about:blank', instance: '/x' } }
}

function workRequest(environmentId: string, state: string, overrides: Record<string, unknown> = {}) {
  return {
    id: `request-${environmentId}`,
    projectId: 'project-1',
    requestKey: `work-${environmentId}`,
    requesterId: 'student-1',
    state,
    generation: 1,
    revision: 1,
    requestedDurationSeconds: 3600,
    requestedResources: { cpuMillicores: 1000, memoryBytes: 2 * 1024 ** 3, storageBytes: 10 * 1024 ** 3 },
    target: { kind: 'environment', environmentId, releaseId: 'release-container', releaseVersion: 3 },
    createdAt: '2026-09-14T09:00:00.000Z',
    updatedAt: '2026-09-14T09:00:00.000Z',
    ...overrides,
  }
}

async function mountAt(query: Record<string, string> = { projectId: 'project-1' }) {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/researcher/environments', component: WorkEnvironmentsView },
      { path: '/researcher/resources', component: { template: '<div />' } },
      { path: '/researcher/ai-policy', component: { template: '<div />' } },
      { path: '/researcher/software', component: { template: '<div />' } },
    ],
  })
  await router.push({ path: '/researcher/environments', query })
  await router.isReady()
  const wrapper = mount({ template: '<RouterView />' }, { global: { plugins: [router] }, attachTo: document.body })
  await flushPromises()
  return { wrapper, router }
}

describe('WorkEnvironmentsView', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    projectsState.selectedProjectId = project.id
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue(problem(404, 'LW_LLM_POLICY_NOT_FOUND') as never)
    vi.mocked(listEnvironments).mockResolvedValue({ data: { items: [workSummary], nextCursor: null }, error: undefined } as never)
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [containerRelease, vmRelease, withdrawnRelease], nextCursor: null }, error: undefined } as never)
    vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: [], error: undefined } as never)
    vi.mocked(listProjectResourceLeases).mockResolvedValue({ data: [], error: undefined } as never)
  })

  it('creates Work without a model: Agent is off with a reason, templates stay selectable', async () => {
    const { wrapper } = await mountAt()
    const note = wrapper.get('[data-testid="agent-availability"]')
    expect(note.text()).toContain('未配置模型')
    expect(note.text()).toContain('手工创建和连接 Work 不受影响')

    const cards = wrapper.findAll('[role="radio"]')
    expect(cards.map((card) => card.text())).toEqual([
      expect.stringContaining('容器模板 v3'),
      expect.stringContaining('虚拟机模板 v1'),
    ])
    expect(wrapper.text()).toContain('容器镜像 labs/python')
    expect(wrapper.text()).toContain('sha256:0123456789ab')
    expect(wrapper.text()).toContain('虚拟机磁盘 · qcow2 · 40 GiB')
    expect(wrapper.find('[data-release-id="release-old"]').exists()).toBe(false)
    expect(wrapper.text()).toContain('已有 Work 不会被自动替换')
    wrapper.unmount()
  })

  it('carries the chosen template version to the resource request step', async () => {
    const { wrapper } = await mountAt()
    expect(wrapper.find('[data-testid="continue-to-resources"]').exists()).toBe(false)

    await wrapper.get('[data-release-id="release-vm"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('[data-release-id="release-vm"]').attributes('aria-checked')).toBe('true')
    const next = wrapper.get('[data-testid="continue-to-resources"]')
    const href = next.attributes('href') ?? ''
    expect(href).toContain('/researcher/resources')
    expect(href).toContain('releaseId=release-vm')
    expect(href).toContain('releaseVersion=1')
    expect(href).toContain('projectId=project-1')
    wrapper.unmount()
  })

  it('lists project Work with its pinned template version and a connect entry', async () => {
    const { wrapper } = await mountAt()
    const row = wrapper.get('[data-environment-id="env-work-1"]')
    expect(row.text()).toContain('分析工作空间')
    expect(row.text()).toContain('固定使用模板 v2')
    expect(row.get('a').text()).toBe('连接')
    expect(row.get('a').attributes('href')).toContain('environmentId=env-work-1')
    expect(listEnvironments).toHaveBeenCalledWith({ query: expect.objectContaining({ projectId: 'project-1', class: 'work' }) })
    wrapper.unmount()
  })

  it('explains the next step honestly when there is no template and no model', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [withdrawnRelease], nextCursor: null }, error: undefined } as never)
    const { wrapper } = await mountAt()
    const empty = wrapper.get('[data-testid="templates-empty"]')
    expect(empty.text()).toContain('当前项目还没有可用的 Work 模板')
    expect(empty.text()).toContain('只能通过 Agent 生成')
    expect(empty.text()).toContain('手工模板需要后端支持')
    expect(empty.find('a[href*="/researcher/software"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('offers Agent template generation only when a model is available', async () => {
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({ data: { binding: { model: 'claude-sonnet' } }, error: undefined } as never)
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined } as never)
    const { wrapper } = await mountAt()
    expect(wrapper.get('[data-testid="agent-availability"]').text()).toContain('claude-sonnet')
    expect(wrapper.get('[data-testid="templates-empty"]').find('a[href*="/researcher/software"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('does not treat an unreadable AI setting as "no model"', async () => {
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue(problem(503, 'LW_CONTROL_UNAVAILABLE') as never)
    const { wrapper } = await mountAt()
    const note = wrapper.get('[data-testid="agent-availability"]').text()
    expect(note).toContain('暂时无法确认 Agent 是否可用')
    expect(note).toContain('LW_CONTROL_UNAVAILABLE')
    expect(note).not.toContain('未配置模型')
    wrapper.unmount()
  })

  it('opens the environment page and loads nothing for the launcher when an environment is chosen', async () => {
    const { wrapper } = await mountAt({ projectId: 'project-1', environmentId: 'env-work-1' })
    expect(wrapper.find('[data-testid="environment-detail"]').exists()).toBe(true)
    expect(listEnvironmentTemplateReleases).not.toHaveBeenCalled()
    expect(getActiveProjectLlmPolicy).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('opens an environment by id from the advanced form', async () => {
    const { wrapper, router } = await mountAt()
    await wrapper.get('#environment-id-input').setValue('env-typed')
    await wrapper.get('.advanced__form').trigger('submit')
    await flushPromises()
    expect(router.currentRoute.value.query).toEqual({ projectId: 'project-1', environmentId: 'env-typed' })
    wrapper.unmount()
  })

  it('shows submitted Work that has no environment yet, with the server state and a progress entry', async () => {
    vi.mocked(listProjectResourceRequests).mockResolvedValue({
      data: [
        workRequest('env-reviewing', 'reviewing'),
        workRequest('env-allocating', 'allocating', { diagnosticCode: 'LW_RESOURCE_WORK_ALLOCATION_BLOCKED' }),
        workRequest('env-creating', 'active'),
        workRequest('env-expiring', 'expiring'),
        // Already listed as Work: shown only in the Work list.
        workRequest('env-work-1', 'active'),
        { ...workRequest('task', 'reviewing'), target: { kind: 'task', taskRunId: 'task-1' } },
      ],
      error: undefined,
    } as never)
    const { wrapper } = await mountAt()
    const preparing = wrapper.get('[data-testid="preparing-list"]')
    expect(preparing.findAll('[data-request-id]').map((row) => row.attributes('data-request-id'))).toEqual([
      'request-env-reviewing',
      'request-env-allocating',
      'request-env-creating',
      'request-env-expiring',
    ])
    expect(preparing.get('[data-request-id="request-env-reviewing"]').text()).toContain('等待管理员审批')
    expect(preparing.get('[data-request-id="request-env-reviewing"] a').attributes('href')).toContain('/researcher/resources')
    const failed = preparing.get('[data-request-id="request-env-allocating"]').text()
    expect(failed).toContain('分配失败，请回收后重新申请。')
    expect(failed).toContain('LW_RESOURCE_WORK_ALLOCATION_BLOCKED')
    expect(preparing.get('[data-request-id="request-env-creating"]').text()).toContain('资源已分配，正在创建环境')
    expect(preparing.get('[data-request-id="request-env-creating"] a').attributes('href')).toContain('environmentId=env-creating')
    expect(preparing.get('[data-request-id="request-env-expiring"]').text()).toContain('资源即将到期，环境尚未创建')
    expect(wrapper.find('[data-testid="ended-list"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('keeps ended requests without Work visible with their reason and a way to apply again', async () => {
    vi.mocked(listProjectResourceRequests).mockResolvedValue({
      data: [
        workRequest('env-rejected', 'rejected'),
        workRequest('env-cancelled', 'cancelled'),
        workRequest('env-expired', 'expired', { diagnosticCode: 'LW_RESOURCE_WORK_ALLOCATION_BLOCKED' }),
      ],
      error: undefined,
    } as never)
    const { wrapper } = await mountAt()
    const ended = wrapper.get('[data-testid="ended-list"]')
    expect(ended.get('[data-request-id="request-env-rejected"]').text()).toContain('管理员已拒绝申请')
    expect(ended.get('[data-request-id="request-env-cancelled"]').text()).toContain('申请已取消')
    const expired = ended.get('[data-request-id="request-env-expired"]')
    expect(expired.text()).toContain('申请已到期，没有创建 Work')
    expect(expired.text()).toContain('分配失败，请回收后重新申请。')
    const links = expired.findAll('a')
    expect(links.map((link) => link.text())).toEqual(['查看资源申请', '用同一模板重新申请'])
    expect(links[1].attributes('href')).toContain('releaseId=release-container')
    expect(links[1].attributes('href')).toContain('releaseVersion=3')
    expect(wrapper.find('[data-testid="preparing-list"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('moves an expiring request to the ended list once the server reports it expired', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
    try {
      vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: [workRequest('env-late', 'expiring')], error: undefined } as never)
      const { wrapper } = await mountAt()
      expect(wrapper.get('[data-testid="preparing-list"]').find('[data-request-id="request-env-late"]').exists()).toBe(true)

      vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: [workRequest('env-late', 'expired')], error: undefined } as never)
      await vi.advanceTimersByTimeAsync(5000)
      await flushPromises()

      expect(wrapper.find('[data-testid="preparing-list"]').exists()).toBe(false)
      expect(wrapper.get('[data-testid="ended-list"]').get('[data-request-id="request-env-late"]').text()).toContain('申请已到期')
      wrapper.unmount()
    } finally {
      vi.useRealTimers()
    }
  })

  it('restores submitted Work from the server after the page is reloaded', async () => {
    vi.mocked(listProjectResourceRequests).mockResolvedValue({
      data: [workRequest('env-allocating', 'allocating'), workRequest('env-rejected', 'rejected')],
      error: undefined,
    } as never)
    const first = await mountAt()
    expect(first.wrapper.find('[data-request-id="request-env-allocating"]').exists()).toBe(true)
    first.wrapper.unmount()

    const second = await mountAt()
    expect(second.wrapper.get('[data-testid="preparing-list"]').text()).toContain('已批准，正在分配资源')
    expect(second.wrapper.get('[data-testid="ended-list"]').text()).toContain('管理员已拒绝申请')
    expect(listProjectResourceRequests).toHaveBeenLastCalledWith({ path: { projectId: 'project-1' } })
    second.wrapper.unmount()
  })

  it('does not claim an active request is still being created when the Work list could not be read', async () => {
    vi.mocked(listEnvironments).mockResolvedValue(problem(503, 'LW_ENVIRONMENT_UNAVAILABLE') as never)
    vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: [workRequest('env-creating', 'active')], error: undefined } as never)
    const { wrapper } = await mountAt()
    expect(wrapper.find('[data-testid="preparing-work"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('reports unreadable resource requests instead of hiding pending Work', async () => {
    vi.mocked(listProjectResourceRequests).mockResolvedValue(problem(503, 'LW_RESOURCE_UNAVAILABLE') as never)
    const { wrapper } = await mountAt()
    const preparing = wrapper.get('[data-testid="preparing-work"]')
    expect(preparing.text()).toContain('无法读取 Work 申请')
    expect(preparing.text()).toContain('LW_RESOURCE_UNAVAILABLE')
    wrapper.unmount()
  })

  it('re-reads the Work list without blanking it while submitted Work is being prepared', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
    try {
      vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: [workRequest('env-new', 'allocating')], error: undefined } as never)
      const { wrapper } = await mountAt()
      expect(wrapper.find('[data-request-id="request-env-new"]').exists()).toBe(true)
      const listCalls = vi.mocked(listEnvironments).mock.calls.length

      vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: [workRequest('env-new', 'active')], error: undefined } as never)
      vi.mocked(listEnvironments).mockResolvedValue({ data: { items: [workSummary, { ...workSummary, id: 'env-new', displayLabel: '新 Work', observedState: 'provisioning' }], nextCursor: null }, error: undefined } as never)
      await vi.advanceTimersByTimeAsync(5000)
      await flushPromises()

      expect(vi.mocked(listEnvironments).mock.calls.length).toBeGreaterThan(listCalls)
      expect(wrapper.find('[data-environment-id="env-work-1"]').exists()).toBe(true)
      expect(wrapper.get('[data-environment-id="env-new"]').text()).toContain('新 Work')
      expect(wrapper.find('[data-testid="preparing-work"]').exists()).toBe(false)
      wrapper.unmount()
    } finally {
      vi.useRealTimers()
    }
  })
})
