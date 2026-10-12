import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import WorkEnvironmentsView from '@/views/researcher/WorkEnvironmentsView.vue'
import { getActiveProjectLlmPolicy, listEnvironments, listEnvironmentTemplateReleases } from '@/generated/contracts'

vi.mock('@/generated/contracts', () => ({
  getActiveProjectLlmPolicy: vi.fn(),
  listEnvironments: vi.fn(),
  listEnvironmentTemplateReleases: vi.fn(),
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
})
