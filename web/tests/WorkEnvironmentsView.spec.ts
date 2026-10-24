import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import WorkEnvironmentsView from '@/views/researcher/WorkEnvironmentsView.vue'
import {
  createEnvironmentTemplateRelease,
  getProjectAgentCapability,
  listEnvironments,
  listEnvironmentTemplateReleases,
  listProjectResourceLeases,
  listProjectResourceRequests,
  listProjectTemplateImages,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', () => ({
  createEnvironmentTemplateRelease: vi.fn(),
  getProjectAgentCapability: vi.fn(),
  listEnvironments: vi.fn(),
  listEnvironmentTemplateReleases: vi.fn(),
  listProjectResourceLeases: vi.fn(),
  listProjectResourceRequests: vi.fn(),
  listProjectTemplateImages: vi.fn(),
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
  purpose: 'work',
  name: 'Python 分析',
  description: '预装 Python 3.12 与 JupyterLab。',
  templateFamilyId: 'python-analysis',
  capabilities: ['terminal', 'files', 'applications', 'agent'],
  publishedAt: '2026-09-12T10:30:00.000Z',
  publishedBy: 'teacher-1',
  manualApproval: { templateFamilyId: 'python-analysis' },
  artifact: { kind: 'catalog_container', id: 'artifact-1', catalog_id: 'image-1', repin_generation: 1, trust_revision: 1, repository: 'registry.example/labs/python', digest: 'sha256:0123456789abcdef0123' },
}
const vmRelease = {
  ...containerRelease,
  id: 'release-vm',
  version: 1,
  runtimeKind: 'virtual_machine',
  name: 'Ubuntu 桌面',
  description: '',
  templateFamilyId: 'ubuntu-desktop',
  capabilities: ['desktop', 'ssh', 'agent'],
  manualApproval: null,
  agentRunId: 'run-1',
  artifact: { kind: 'virtual_machine', id: 'artifact-2', format: 'qcow2', base_disk: { binding: 'disk', capacityBytes: 40 * 1024 ** 3, sourceRegistryDigest: 'sha256:x' } },
}
const withdrawnRelease = { ...containerRelease, id: 'release-old', version: 1, withdrawal: { reasonCode: 'TEACHER_WITHDRAWN' } }
const experimentRelease = { ...containerRelease, id: 'release-experiment', version: 2, purpose: 'experiment', name: '课程实验' }

const containerImage = {
  catalogId: '0197f0e0-0000-7000-8000-000000000001',
  kind: 'container',
  binding: 'ubuntu-24.04-v1',
  sourceReference: 'registry.example/labweaver-system/ubuntu:24.04',
  resolvedDigest: 'sha256:9a3e7c6d1f2b4a8d3e6f7a1b0c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1',
  mediaType: 'application/vnd.oci.image.manifest.v1+json',
  sizeBytes: 1024,
  status: 'active',
  trustRevision: 4,
  repinGeneration: 2,
  pinnedAt: '2026-09-01T00:00:00.000Z',
  updatedAt: '2026-09-01T00:00:00.000Z',
}
const vmImage = {
  ...containerImage,
  catalogId: '0197f0e0-0000-7000-8000-000000000002',
  kind: 'virtual_machine',
  binding: 'ubuntu-24.04-vm-v1',
  resolvedDigest: 'sha256:4b1d2f8a6c0e9d7b5a3f1e8c6d4b2a0f9e7c5d3b1a8f6e4c2d0b9a7f5e3c1d0b',
  capacityBytes: 10 * 1024 ** 3,
  format: 'qcow2',
  repinGeneration: 1,
}
const disabledImage = { ...containerImage, catalogId: '0197f0e0-0000-7000-8000-000000000003', binding: 'alpine-v1', status: 'disabled' }

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

function field(wrapper: VueWrapper, label: string) {
  const match = wrapper.findAll('.manual-form label.field').find((item) => item.find('span').text() === label)
  if (!match) throw new Error(`field ${label} not found`)
  return match.get('input, textarea')
}

describe('WorkEnvironmentsView', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    projectsState.selectedProjectId = project.id
    vi.mocked(getProjectAgentCapability).mockResolvedValue({ data: { status: 'unconfigured', reasonCode: 'LW_AGENT_MODEL_NOT_CONFIGURED' }, error: undefined } as never)
    vi.mocked(listEnvironments).mockResolvedValue({ data: { items: [workSummary], nextCursor: null }, error: undefined } as never)
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [containerRelease, vmRelease, withdrawnRelease, experimentRelease], nextCursor: null }, error: undefined } as never)
    vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined } as never)
    vi.mocked(listProjectResourceLeases).mockResolvedValue({ data: [], error: undefined } as never)
    vi.mocked(listProjectTemplateImages).mockResolvedValue({ data: { entries: [containerImage, vmImage, disabledImage] }, error: undefined } as never)
  })

  it('lists only Work templates with name, description, capabilities and source; Agent is off with a reason', async () => {
    const { wrapper } = await mountAt()
    const note = wrapper.get('[data-testid="agent-availability"]')
    expect(note.attributes('data-agent-state')).toBe('unconfigured')
    expect(note.text()).toContain('Agent 已关闭：项目还没有配置模型')
    expect(note.text()).toContain('手工创建和连接 Work 不受影响')
    expect(getProjectAgentCapability).toHaveBeenCalledWith({ path: { projectId: 'project-1' } })

    const cards = wrapper.findAll('[role="radio"]')
    expect(cards.map((card) => card.attributes('data-release-id'))).toEqual(['release-container', 'release-vm'])
    const container = wrapper.get('[data-release-id="release-container"]')
    expect(container.text()).toContain('Python 分析 v3')
    expect(container.text()).toContain('容器 · 手工发布')
    expect(container.text()).toContain('预装 Python 3.12 与 JupyterLab。')
    expect(container.findAll('[data-capability]').map((chip) => chip.text())).toEqual(['浏览器终端', '文件', 'Web 应用', 'Agent（项目当前不可用）'])
    expect(container.text()).toContain('容器镜像 labs/python · sha256:0123456789ab')
    const vm = wrapper.get('[data-release-id="release-vm"]')
    expect(vm.text()).toContain('虚拟机 · Agent 生成并审核')
    expect(vm.text()).toContain('虚拟机磁盘 · qcow2 · 40 GiB')
    // Withdrawn releases and course experiments never create Work.
    expect(wrapper.find('[data-release-id="release-old"]').exists()).toBe(false)
    expect(wrapper.find('[data-release-id="release-experiment"]').exists()).toBe(false)
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

  it('points to manual publication when there is no Work template and no model, and opens the form', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [withdrawnRelease, experimentRelease], nextCursor: null }, error: undefined } as never)
    const { wrapper } = await mountAt()
    const empty = wrapper.get('[data-testid="templates-empty"]')
    expect(empty.text()).toContain('当前项目还没有可用的 Work 模板')
    expect(empty.text()).toContain('从平台镜像直接发布模板，不需要模型')
    expect(empty.find('a[href*="/researcher/software"]').exists()).toBe(false)
    expect(wrapper.get('[data-testid="manual-section"]').attributes('open')).toBeDefined()
    // Only active pinned images are offered.
    expect(wrapper.findAll('[data-image-id]').map((item) => item.attributes('data-image-id'))).toEqual([containerImage.catalogId, vmImage.catalogId])
    wrapper.unmount()
  })

  it('offers Agent template generation only when a model is configured', async () => {
    vi.mocked(getProjectAgentCapability).mockResolvedValue({ data: { status: 'configured', model: 'claude-sonnet' }, error: undefined } as never)
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined } as never)
    const { wrapper } = await mountAt()
    expect(wrapper.get('[data-testid="agent-availability"]').text()).toContain('claude-sonnet')
    expect(wrapper.get('[data-testid="templates-empty"]').find('a[href*="/researcher/software"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('says why a configured model binding is unusable without blocking Work', async () => {
    vi.mocked(getProjectAgentCapability).mockResolvedValue({ data: { status: 'unavailable', reasonCode: 'LW_AGENT_MODEL_BINDING_UNAVAILABLE' }, error: undefined } as never)
    const { wrapper } = await mountAt()
    const note = wrapper.get('[data-testid="agent-availability"]').text()
    expect(note).toContain('项目配置的模型在当前平台部署中不可用')
    expect(wrapper.findAll('[role="radio"]')).toHaveLength(2)
    wrapper.unmount()
  })

  it('does not treat an unreadable Agent configuration as "no model"', async () => {
    vi.mocked(getProjectAgentCapability).mockResolvedValue({
      data: undefined,
      error: { status: 503, diagnosticCode: 'LW_AGENT_CONFIGURATION_UNREADABLE', detail: 'config unreadable', requestId: 'req-agent', retryable: true },
    } as never)
    const { wrapper } = await mountAt()
    const note = wrapper.get('[data-testid="agent-availability"]')
    expect(note.attributes('data-agent-state')).toBe('unreadable')
    expect(note.text()).toContain('暂时无法确认 Agent 是否可用：平台无法读取项目的 Agent 配置。')
    expect(note.text()).toContain('读取失败不代表项目没有模型')
    expect(note.text()).toContain('LW_AGENT_CONFIGURATION_UNREADABLE')
    expect(note.text()).toContain('req-agent')
    expect(note.text()).not.toContain('Agent 已关闭')
    // Templates stay selectable; the Agent chip is shown as unavailable.
    expect(wrapper.findAll('[role="radio"]')).toHaveLength(2)
    expect(wrapper.get('[data-release-id="release-container"] [data-capability="agent"]').text()).toContain('项目当前不可用')
    wrapper.unmount()
  })

  it('publishes a reviewed manual container template once per reviewed content and reloads the template list', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined } as never)
    vi.mocked(createEnvironmentTemplateRelease)
      .mockResolvedValueOnce({ data: undefined, error: { status: 503, diagnosticCode: 'LW_CONTROL_UNAVAILABLE', detail: 'control down', requestId: 'req-publish', retryable: true } } as never)
      .mockResolvedValueOnce({ data: { operationId: 'op-1', statusUrl: '/api/v1/operations/op-1' }, error: undefined } as never)
    const { wrapper } = await mountAt()
    const publish = () => wrapper.get('[data-testid="manual-publish"]')
    expect((publish().element as HTMLButtonElement).disabled).toBe(true)
    expect(wrapper.get('[data-testid="manual-problem"]').text()).toContain('请选择一个平台镜像')

    await wrapper.get(`[data-image-id="${containerImage.catalogId}"]`).trigger('click')
    await field(wrapper, '模板名称').setValue('Python 分析')
    await field(wrapper, '模板系列标识').setValue('python-analysis')
    await field(wrapper, '说明（用途、预装软件）').setValue('预装 Python 3.12。')
    await field(wrapper, '运行位置（provider binding）').setValue('kubernetes-work')
    await field(wrapper, '安全配置（security profile）').setValue('restricted-v1')
    await field(wrapper, '审核说明').setValue('已核对镜像与端口。')
    expect(wrapper.get('[data-testid="manual-problem"]').text()).toContain('请先确认')
    await wrapper.get('[data-testid="manual-confirm"]').setValue(true)
    expect(wrapper.find('[data-testid="manual-problem"]').exists()).toBe(false)

    await wrapper.get('.manual-form').trigger('submit')
    await flushPromises()
    expect(createEnvironmentTemplateRelease).toHaveBeenCalledTimes(1)
    const first = vi.mocked(createEnvironmentTemplateRelease).mock.calls[0][0]
    expect(first.path).toEqual({ projectId: 'project-1' })
    expect(first.body).toMatchObject({
      projectId: 'project-1',
      runtimeKind: 'container',
      manual: {
        platformImageId: containerImage.catalogId,
        platformImageGeneration: 2,
        templateFamilyId: 'python-analysis',
        description: '预装 Python 3.12。',
        reason: '已核对镜像与端口。',
        confirmed: true,
        spec: {
          class: 'work',
          name: 'Python 分析',
          entries: [{ name: 'app', protocol: 'http', servicePort: 8080 }],
          network: { mode: 'deny_all' },
          security: { rootFilesystemPolicy: 'read_only_required', securityProfileBinding: 'restricted-v1', userPolicy: 'non_root_required' },
          runtime: { kind: 'container', provider_binding: 'kubernetes-work', service_port: 8080, terminal: { executable: '/bin/bash', args: [], workingDirectory: '/workspace' } },
          retention: { class: 'run_evidence', disposition: 'delete', policyRevision: 1 },
        },
      },
    })
    expect(first.body).not.toHaveProperty('candidateId')
    expect(first.body.manual?.spec.runtime).not.toHaveProperty('build_context')
    const banner = wrapper.get('[data-testid="manual-publisher"] .diagnostic-banner')
    expect(banner.text()).toContain('req-publish')
    const releaseReads = vi.mocked(listEnvironmentTemplateReleases).mock.calls.length

    // Retrying the same reviewed content reuses its Idempotency-Key and retention identity.
    await banner.findAll('button').find((button) => button.text() === '重试')!.trigger('click')
    await flushPromises()
    const second = vi.mocked(createEnvironmentTemplateRelease).mock.calls[1][0]
    expect(second.headers['Idempotency-Key']).toBe(first.headers['Idempotency-Key'])
    expect(second.body.manual?.spec.retention.policyId).toBe(first.body.manual?.spec.retention.policyId)
    expect(wrapper.get('[data-testid="manual-accepted"]').text()).toContain('发布已受理')
    expect(vi.mocked(listEnvironmentTemplateReleases).mock.calls.length).toBeGreaterThan(releaseReads)
    wrapper.unmount()
  })

  it('pins a VM template to the reviewed catalog disk with SSH on port 22', async () => {
    vi.mocked(createEnvironmentTemplateRelease).mockResolvedValue({ data: { operationId: 'op-1', statusUrl: '/x' }, error: undefined } as never)
    const { wrapper } = await mountAt()
    await wrapper.get('[data-testid="manual-section"] > summary').trigger('click')
    wrapper.get<HTMLDetailsElement>('[data-testid="manual-section"]').element.open = true
    await wrapper.get('[data-testid="manual-section"]').trigger('toggle')
    await flushPromises()
    await wrapper.get(`[data-image-id="${vmImage.catalogId}"]`).trigger('click')
    await field(wrapper, '模板名称').setValue('Ubuntu 桌面')
    await field(wrapper, '模板系列标识').setValue('ubuntu-desktop')
    await field(wrapper, '运行位置（provider binding）').setValue('kubevirt-primary-v1')
    await field(wrapper, '安全配置（security profile）').setValue('kubevirt-restricted-v1')
    await field(wrapper, '审核说明').setValue('已核对磁盘。')
    await wrapper.get('[data-testid="manual-confirm"]').setValue(true)
    expect(wrapper.get('[data-testid="manual-problem"]').text()).toContain('磁盘存储类')
    await field(wrapper, '磁盘存储类（storage class）').setValue('vm-rwo-primary-v1')
    await wrapper.get('.manual-form').trigger('submit')
    await flushPromises()
    expect(vi.mocked(createEnvironmentTemplateRelease).mock.calls[0][0].body).toMatchObject({
      runtimeKind: 'virtual_machine',
      manual: {
        platformImageId: vmImage.catalogId,
        platformImageGeneration: 1,
        spec: {
          network: { mode: 'deny_all' },
          entries: [{ name: 'ssh', protocol: 'ssh', servicePort: 22 }],
          security: { rootFilesystemPolicy: 'mutable_required' },
          runtime: {
            kind: 'virtual_machine',
            provider_binding: 'kubevirt-primary-v1',
            storage_class_binding: 'vm-rwo-primary-v1',
            ssh_port: 22,
            base_disk: { binding: 'ubuntu-24.04-vm-v1', capacityBytes: 10 * 1024 ** 3, sourceRegistryDigest: vmImage.resolvedDigest },
          },
        },
      },
    })
    wrapper.unmount()
  })

  it('opens the environment page and loads nothing for the launcher when an environment is chosen', async () => {
    const { wrapper } = await mountAt({ projectId: 'project-1', environmentId: 'env-work-1' })
    expect(wrapper.find('[data-testid="environment-detail"]').exists()).toBe(true)
    expect(listEnvironmentTemplateReleases).not.toHaveBeenCalled()
    expect(getProjectAgentCapability).not.toHaveBeenCalled()
    expect(listProjectTemplateImages).not.toHaveBeenCalled()
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
      data: { items: [
        workRequest('env-reviewing', 'reviewing'),
        workRequest('env-allocating', 'allocating', { diagnosticCode: 'LW_RESOURCE_WORK_ALLOCATION_BLOCKED' }),
        workRequest('env-creating', 'active'),
        workRequest('env-expiring', 'expiring'),
        workRequest('env-work-1', 'active'),
        { ...workRequest('task', 'reviewing'), target: { kind: 'task', taskRunId: 'task-1' } },
      ], nextCursor: null },
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
      data: { items: [
        workRequest('env-rejected', 'rejected'),
        workRequest('env-cancelled', 'cancelled'),
        workRequest('env-expired', 'expired', { diagnosticCode: 'LW_RESOURCE_WORK_ALLOCATION_BLOCKED' }),
      ], nextCursor: null },
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
      vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: { items: [workRequest('env-late', 'expiring')], nextCursor: null }, error: undefined } as never)
      const { wrapper } = await mountAt()
      expect(wrapper.get('[data-testid="preparing-list"]').find('[data-request-id="request-env-late"]').exists()).toBe(true)

      vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: { items: [workRequest('env-late', 'expired')], nextCursor: null }, error: undefined } as never)
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
      data: { items: [workRequest('env-allocating', 'allocating'), workRequest('env-rejected', 'rejected')], nextCursor: null },
    } as never)
    const first = await mountAt()
    expect(first.wrapper.find('[data-request-id="request-env-allocating"]').exists()).toBe(true)
    first.wrapper.unmount()

    const second = await mountAt()
    expect(second.wrapper.get('[data-testid="preparing-list"]').text()).toContain('已批准，正在分配资源')
    expect(second.wrapper.get('[data-testid="ended-list"]').text()).toContain('管理员已拒绝申请')
    expect(listProjectResourceRequests).toHaveBeenLastCalledWith({ path: { projectId: 'project-1' }, query: { limit: 100 } })
    second.wrapper.unmount()
  })

  it('does not claim an active request is still being created when the Work list could not be read', async () => {
    vi.mocked(listEnvironments).mockResolvedValue(problem(503, 'LW_ENVIRONMENT_UNAVAILABLE') as never)
    vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: { items: [workRequest('env-creating', 'active')], nextCursor: null }, error: undefined } as never)
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
      vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: { items: [workRequest('env-new', 'allocating')], nextCursor: null }, error: undefined } as never)
      const { wrapper } = await mountAt()
      expect(wrapper.find('[data-request-id="request-env-new"]').exists()).toBe(true)
      const listCalls = vi.mocked(listEnvironments).mock.calls.length

      vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: { items: [workRequest('env-new', 'active')], nextCursor: null }, error: undefined } as never)
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
