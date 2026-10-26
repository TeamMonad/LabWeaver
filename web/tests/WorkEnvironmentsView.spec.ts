import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import WorkEnvironmentsView from '@/views/researcher/WorkEnvironmentsView.vue'
import { useAuth } from '@/composables/useAuth'
import { listPendingCommands, syncPendingCommandOwner } from '@/utils/pendingCommands'
import {
  createEnvironmentTemplateRelease,
  getProjectAgentCapability,
  getProjectAvailableActions,
  getProjectManualTemplateOptions,
  listEnvironments,
  listEnvironmentTemplateReleases,
  listProjectResourceLeases,
  listProjectResourceRequests,
  listProjectTemplateImages,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', () => ({
  createEnvironmentTemplateRelease: vi.fn(),
  getProjectAgentCapability: vi.fn(),
  getProjectAvailableActions: vi.fn(),
  getProjectManualTemplateOptions: vi.fn(),
  listEnvironments: vi.fn(),
  listEnvironmentTemplateReleases: vi.fn(),
  listProjectResourceLeases: vi.fn(),
  listProjectResourceRequests: vi.fn(),
  listProjectTemplateImages: vi.fn(),
  withdrawEnvironmentTemplateRelease: vi.fn(),
}))

function signIn(actorId: string | null, roles: string[] = ['student']) {
  useAuth().user.value = actorId ? { expired: false, profile: { actor_id: actorId, roles } } as never : null
}

function grantActions(actions: string[]) {
  return { data: { projectId: 'project-1', actorId: 'actor', membershipRevision: 1, validUntil: '2099-01-01T00:00:00.000Z', actions }, error: undefined } as never
}

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

const manualOptions = {
  runtime: { providers: [
    { binding: 'kubernetes-work', label: '校园教学容器', description: '工作区使用持久存储。', runtimeKind: 'container', storageClasses: [] },
    { binding: 'kubevirt-primary-v1', label: '校园教学虚拟机', description: '独立虚拟机磁盘。', runtimeKind: 'virtual_machine', storageClasses: [{ binding: 'vm-rwo-primary-v1', label: '教学持久磁盘', description: '最大 128 GiB。', maxCapacityBytes: 128 * 1024 ** 3, seededDisks: [] }] },
  ] },
  retention: { policyId: '0197f0e0-0000-7000-8000-000000000099', policyRevision: 1, label: '校园材料保留策略', description: '最长保留 365 天。', maxRetentionSeconds: 31536000 },
}

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
  return match.get('input, textarea, select')
}

describe('WorkEnvironmentsView', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    window.sessionStorage.clear()
    signIn('owner-1')
    projectsState.selectedProjectId = project.id
    vi.mocked(getProjectAvailableActions).mockResolvedValue(grantActions(['browse', 'edit', 'start', 'stop', 'grant_access']))
    vi.mocked(getProjectAgentCapability).mockResolvedValue({ data: { status: 'unconfigured', reasonCode: 'LW_AGENT_MODEL_NOT_CONFIGURED' }, error: undefined } as never)
    vi.mocked(listEnvironments).mockResolvedValue({ data: { items: [workSummary], nextCursor: null }, error: undefined } as never)
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [containerRelease, vmRelease, withdrawnRelease, experimentRelease], nextCursor: null }, error: undefined } as never)
    vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined } as never)
    vi.mocked(listProjectResourceLeases).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined } as never)
    vi.mocked(listProjectTemplateImages).mockResolvedValue({ data: { entries: [containerImage, vmImage, disabledImage] }, error: undefined } as never)
    vi.mocked(getProjectManualTemplateOptions).mockResolvedValue({ data: manualOptions, error: undefined } as never)
  })

  it('uses labelled provider and retention selects from the server and scopes storage to the selected runtime', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined } as never)
    const { wrapper } = await mountAt()
    await wrapper.get(`[data-image-id="${containerImage.catalogId}"]`).trigger('click')
    expect(getProjectManualTemplateOptions).toHaveBeenCalledWith({ path: { projectId: 'project-1' } })
    const provider = field(wrapper, '运行位置')
    expect(provider.element.tagName).toBe('SELECT')
    expect(provider.text()).toContain('校园教学容器 · kubernetes-work')
    expect(provider.text()).not.toContain('kubevirt-primary-v1')
    expect(field(wrapper, '材料保留策略').text()).toContain('校园材料保留策略')
    await provider.setValue('kubernetes-work')
    expect(wrapper.text()).toContain('工作区使用持久存储。')
    await wrapper.get(`[data-image-id="${vmImage.catalogId}"]`).trigger('click')
    expect((wrapper.get('[data-testid="manual-publish"]').element as HTMLButtonElement).disabled).toBe(true)
    await field(wrapper, '运行位置').setValue('kubevirt-primary-v1')
    expect(field(wrapper, '磁盘存储').text()).toContain('教学持久磁盘 · vm-rwo-primary-v1')
    await field(wrapper, '磁盘存储').setValue('vm-rwo-primary-v1')
    expect(wrapper.text()).toContain('最大 128 GiB')
    wrapper.unmount()
  })

  it('does not offer free-text fallback or publication when runtime options are empty', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined } as never)
    vi.mocked(getProjectManualTemplateOptions).mockResolvedValue({ data: { ...manualOptions, runtime: { providers: [] } }, error: undefined } as never)
    const { wrapper } = await mountAt()
    expect(wrapper.text()).toContain('当前没有可用的运行位置')
    expect(wrapper.find('.manual-form').exists()).toBe(false)
    expect(createEnvironmentTemplateRelease).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('shows an actionable options error and reloads it without treating unreadable config as an empty catalog', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined } as never)
    const unavailable = problem(503, 'LW_CONTROL_DOWNSTREAM_UNAVAILABLE')
    unavailable.error.retryable = true
    vi.mocked(getProjectManualTemplateOptions).mockResolvedValueOnce(unavailable as never)
    const { wrapper } = await mountAt()
    expect(wrapper.find('.manual-form').exists()).toBe(false)
    expect(wrapper.text()).not.toContain('当前没有可用的运行位置')
    const banner = wrapper.get('[data-testid="manual-publisher"] .diagnostic-banner')
    expect(banner.text()).toContain('req-1')
    await banner.findAll('button').find((button) => button.text() === '重试')!.trigger('click')
    await flushPromises()
    expect(wrapper.find('.manual-form').exists()).toBe(true)
    wrapper.unmount()
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
    await field(wrapper, '运行位置').setValue('kubernetes-work')
    await field(wrapper, '安全配置标签').setValue('restricted-v1')
    await field(wrapper, '材料保留策略').setValue(manualOptions.retention.policyId)
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
          security: { readOnlyRootFilesystem: false, securityProfileBinding: 'restricted-v1', runAsUser: null },
          runtime: { kind: 'container', provider_binding: 'kubernetes-work', service_port: 8080, terminal: { executable: '/bin/bash', args: [], workingDirectory: '/workspace' } },
          retention: { class: 'run_evidence', disposition: 'delete', policyRevision: 1, policyId: manualOptions.retention.policyId },
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
    await field(wrapper, '运行位置').setValue('kubevirt-primary-v1')
    await field(wrapper, '安全配置标签').setValue('kubevirt-restricted-v1')
    await field(wrapper, '材料保留策略').setValue(manualOptions.retention.policyId)
    await field(wrapper, '审核说明').setValue('已核对磁盘。')
    await wrapper.get('[data-testid="manual-confirm"]').setValue(true)
    expect(wrapper.get('[data-testid="manual-problem"]').text()).toContain('磁盘存储')
    await field(wrapper, '磁盘存储').setValue('vm-rwo-primary-v1')
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
          security: { readOnlyRootFilesystem: false },
          runtime: {
            kind: 'virtual_machine',
            provider_binding: 'kubevirt-primary-v1',
            storage_class_binding: 'vm-rwo-primary-v1',
            ssh_port: 22,
            base_disk: { binding: 'ubuntu-24.04-vm-v1', capacityBytes: 10 * 1024 ** 3, sourceRegistryDigest: `docker://registry.example/labweaver-system/ubuntu@${vmImage.resolvedDigest}` },
          },
        },
      },
    })
    wrapper.unmount()
  })

  async function fillContainerDraft(wrapper: VueWrapper, name = 'Python 分析') {
    await wrapper.get(`[data-image-id="${containerImage.catalogId}"]`).trigger('click')
    await field(wrapper, '模板名称').setValue(name)
    await field(wrapper, '模板系列标识').setValue('python-analysis')
    await field(wrapper, '运行位置').setValue('kubernetes-work')
    await field(wrapper, '安全配置标签').setValue('restricted-v1')
    await field(wrapper, '材料保留策略').setValue(manualOptions.retention.policyId)
    await field(wrapper, '审核说明').setValue('已核对镜像与端口。')
    await wrapper.get('[data-testid="manual-confirm"]').setValue(true)
  }

  async function mountEmpty() {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined } as never)
    return mountAt()
  }

  function publishKeys() {
    return vi.mocked(createEnvironmentTemplateRelease).mock.calls.map(([options]) => options.headers['Idempotency-Key'])
  }

  describe('manual publication permissions', () => {
    it('does not offer publication to a read-only member', async () => {
      signIn('member-1')
      vi.mocked(getProjectAvailableActions).mockResolvedValue(grantActions(['browse']))
      const { wrapper } = await mountEmpty()
      expect(wrapper.get('[data-testid="manual-permission-hint"]').text()).toContain('发布 Work 模板')
      expect(wrapper.find('.manual-form').exists()).toBe(false)
      wrapper.unmount()
    })

    it('explains that an editor without owner, administrator or member management cannot publish', async () => {
      signIn('editor-1')
      vi.mocked(getProjectAvailableActions).mockResolvedValue(grantActions(['browse', 'edit', 'start', 'stop']))
      const { wrapper } = await mountEmpty()
      expect(wrapper.get('[data-testid="manual-permission-hint"]').text()).toContain('项目所有者、平台管理员或项目管理（管理成员）职责')
      expect(wrapper.find('.manual-form').exists()).toBe(false)
      wrapper.unmount()
    })

    it.each([
      ['a manager with member management', 'manager-1', ['student'], ['browse', 'edit', 'manage_members']],
      ['the project owner', 'owner-1', ['student'], ['browse', 'edit']],
      ['a platform administrator', 'admin-1', ['admin'], ['browse', 'edit']],
    ])('offers publication to %s', async (_label, actor, roles, actions) => {
      signIn(actor, roles)
      vi.mocked(getProjectAvailableActions).mockResolvedValue(grantActions(actions))
      const { wrapper } = await mountEmpty()
      expect(wrapper.find('[data-testid="manual-permission-hint"]').exists()).toBe(false)
      expect(wrapper.find('.manual-form').exists()).toBe(true)
      wrapper.unmount()
    })

    it('blocks submission while the project actions are unknown', async () => {
      vi.mocked(getProjectAvailableActions).mockResolvedValue({ data: undefined, error: { status: 503, diagnosticCode: 'LW_CONTROL_UNAVAILABLE', detail: 'down', retryable: true } } as never)
      const { wrapper } = await mountEmpty()
      expect(wrapper.find('[data-testid="manual-permission-hint"]').exists()).toBe(true)
      expect(wrapper.find('[data-testid="manual-publish"]').exists()).toBe(false)
      wrapper.unmount()
    })

    it('re-reads the project actions after a 403, keeps the command and replays it once permission returns', async () => {
      vi.mocked(createEnvironmentTemplateRelease).mockResolvedValueOnce({ data: undefined, error: { status: 403, diagnosticCode: 'LW_PROJECT_GOVERNANCE_DENIED', detail: 'denied', retryable: false } } as never)
      const first = await mountEmpty()
      await fillContainerDraft(first.wrapper)
      vi.mocked(getProjectAvailableActions).mockResolvedValue(grantActions(['browse']))
      const reads = vi.mocked(getProjectAvailableActions).mock.calls.length
      await first.wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      expect(vi.mocked(getProjectAvailableActions).mock.calls.length).toBeGreaterThan(reads)
      await vi.waitFor(() => expect(first.wrapper.find('[data-testid="manual-permission-hint"]').exists()).toBe(true))
      // Permission is checked before the idempotency ledger: a 403 does not prove an earlier attempt failed.
      expect(first.wrapper.get('[data-testid="manual-unconfirmed"]').text()).toContain('发布结果待确认')
      expect(Object.keys(window.sessionStorage).filter((key) => key.startsWith('labweaver.pending-command:owner-1:manual-work-template:'))).toHaveLength(1)
      first.wrapper.unmount()

      vi.mocked(getProjectAvailableActions).mockResolvedValue(grantActions(['browse', 'edit']))
      vi.mocked(createEnvironmentTemplateRelease).mockResolvedValueOnce({ data: { operationId: 'op-1', statusUrl: '/x' }, error: undefined } as never)
      const second = await mountEmpty()
      await second.wrapper.get('[data-testid="manual-confirm"]').setValue(true)
      await second.wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      const [denied, replay] = vi.mocked(createEnvironmentTemplateRelease).mock.calls.map(([options]) => options)
      expect(replay.headers['Idempotency-Key']).toBe(denied.headers['Idempotency-Key'])
      expect(replay.body.manual?.spec.retention.policyId).toBe(denied.body.manual?.spec.retention.policyId)
      second.wrapper.unmount()
    })
  })

  describe('manual publication outcome', () => {
    it('keeps the identity after a retryable 409 operation in progress', async () => {
      vi.mocked(createEnvironmentTemplateRelease)
        .mockResolvedValueOnce({ data: undefined, error: { status: 409, diagnosticCode: 'LW_OPERATION_IN_PROGRESS', detail: 'in progress', retryable: true } } as never)
        .mockResolvedValueOnce({ data: { operationId: 'op-1', statusUrl: '/x' }, error: undefined } as never)
      const { wrapper } = await mountEmpty()
      await fillContainerDraft(wrapper)
      await wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      expect(wrapper.get('[data-testid="manual-unconfirmed"]').exists()).toBe(true)
      await wrapper.get('[data-testid="manual-publisher"] .diagnostic-banner').findAll('button').find((button) => button.text() === '重试')!.trigger('click')
      await flushPromises()
      const keys = publishKeys()
      expect(keys[1]).toBe(keys[0])
      expect(wrapper.get('[data-testid="manual-accepted"]').exists()).toBe(true)
      wrapper.unmount()
    })

    it('drops the identity when the reviewed content is rejected before anything is committed', async () => {
      vi.mocked(createEnvironmentTemplateRelease).mockResolvedValue({ data: undefined, error: { status: 422, diagnosticCode: 'LW_CONTRACT_DOCUMENT_INVALID', detail: 'invalid', retryable: false } } as never)
      const { wrapper } = await mountEmpty()
      await fillContainerDraft(wrapper)
      await wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      expect(wrapper.find('[data-testid="manual-unconfirmed"]').exists()).toBe(false)
      expect(Object.keys(window.sessionStorage).filter((key) => key.startsWith('labweaver.pending-command:'))).toEqual([])
      await wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      const keys = publishKeys()
      expect(keys[1]).not.toBe(keys[0])
      wrapper.unmount()
    })

    it('lets a late answer to an older command neither delete a newer command nor change the reopened form', async () => {
      let finishA: (value: unknown) => void = () => undefined
      let finishB: (value: unknown) => void = () => undefined
      vi.mocked(createEnvironmentTemplateRelease)
        .mockImplementationOnce(() => new Promise((resolve) => { finishA = resolve }) as never)
        .mockImplementationOnce(() => new Promise((resolve) => { finishB = resolve }) as never)
      const { wrapper } = await mountEmpty()
      await fillContainerDraft(wrapper)
      await wrapper.get('.manual-form').trigger('submit')
      await flushPromises()

      // Collapse while A is in flight, reopen and submit different content as B.
      const section = wrapper.get<HTMLDetailsElement>('[data-testid="manual-section"]')
      section.element.open = false
      await section.trigger('toggle')
      section.element.open = true
      await section.trigger('toggle')
      await flushPromises()
      await field(wrapper, '模板名称').setValue('Python 分析（第二版）')
      await wrapper.get('[data-testid="manual-confirm"]').setValue(true)
      await wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      const [a, b] = vi.mocked(createEnvironmentTemplateRelease).mock.calls.map(([options]) => options)
      expect(b.headers['Idempotency-Key']).not.toBe(a.headers['Idempotency-Key'])

      // Answers arrive out of order: B is unconfirmed first, then A succeeds.
      finishB({ data: undefined, error: { status: 503, diagnosticCode: 'LW_CONTROL_UNAVAILABLE', detail: 'down', retryable: true } })
      await flushPromises()
      finishA({ data: { operationId: 'op-a', statusUrl: '/a' }, error: undefined })
      await flushPromises()

      const stored = Object.entries(window.sessionStorage)
        .filter(([key]) => key.startsWith('labweaver.pending-command:owner-1:manual-work-template:'))
        .map(([, value]) => JSON.parse(value))
      expect(stored).toHaveLength(1)
      expect(stored[0].idempotencyKey).toBe(b.headers['Idempotency-Key'])
      expect(stored[0].data.draft.retentionPolicyId).toBe(b.body.manual?.spec.retention.policyId)
      // The reopened form still shows B's unconfirmed outcome, not A's acceptance.
      expect(wrapper.find('[data-testid="manual-accepted"]').exists()).toBe(false)
      expect(wrapper.get('[data-testid="manual-unconfirmed"]').exists()).toBe(true)
      expect((field(wrapper, '模板名称').element as HTMLInputElement).value).toBe('Python 分析（第二版）')
      wrapper.unmount()
    })

    it('keeps using a newer memory-only command over an older stored one, also after the older answer arrives late', async () => {
      let finishA: (value: unknown) => void = () => undefined
      vi.mocked(createEnvironmentTemplateRelease)
        .mockImplementationOnce(() => new Promise((resolve) => { finishA = resolve }) as never)
        .mockRejectedValueOnce(new TypeError('Failed to fetch'))
        .mockRejectedValueOnce(new TypeError('Failed to fetch'))
        .mockRejectedValueOnce(new TypeError('Failed to fetch'))
      const { wrapper } = await mountEmpty()
      await fillContainerDraft(wrapper)
      await wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      // A (K1) is stored; from now on session storage refuses every write.
      const quota = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new DOMException('full', 'QuotaExceededError') })
      try {
        const section = wrapper.get<HTMLDetailsElement>('[data-testid="manual-section"]')
        section.element.open = false
        await section.trigger('toggle')
        section.element.open = true
        await section.trigger('toggle')
        await flushPromises()
        await field(wrapper, '模板名称').setValue('Python 分析（第二版）')
        await wrapper.get('[data-testid="manual-confirm"]').setValue(true)
        await wrapper.get('.manual-form').trigger('submit')
        await flushPromises()
        // Unchanged B again: must replay B's memory-only key and retention identity, not A's.
        await wrapper.get('[data-testid="manual-confirm"]').setValue(true)
        await wrapper.get('.manual-form').trigger('submit')
        await flushPromises()

        // A's answer arrives late; its cleanup must not remove B.
        finishA({ data: { operationId: 'op-a', statusUrl: '/a' }, error: undefined })
        await flushPromises()
        await wrapper.get('[data-testid="manual-confirm"]').setValue(true)
        await wrapper.get('.manual-form').trigger('submit')
        await flushPromises()

        const [a, b1, b2, b3] = vi.mocked(createEnvironmentTemplateRelease).mock.calls.map(([options]) => options)
        expect(b1.headers['Idempotency-Key']).not.toBe(a.headers['Idempotency-Key'])
        expect(b2.headers['Idempotency-Key']).toBe(b1.headers['Idempotency-Key'])
        expect(b3.headers['Idempotency-Key']).toBe(b1.headers['Idempotency-Key'])
        const policyId = (options: typeof a) => options.body.manual?.spec.retention.policyId
        expect(policyId(b2)).toBe(policyId(b1))
        expect(policyId(b3)).toBe(policyId(b1))
        expect(policyId(b1)).toBe(manualOptions.retention.policyId)
        // The older stored A no longer competes with B.
        expect(Object.keys(window.sessionStorage).filter((key) => key.startsWith('labweaver.pending-command:owner-1:manual-work-template:'))).toEqual([])
        expect(listPendingCommands('owner-1', 'manual-work-template:')).toEqual([
          expect.objectContaining({ persisted: false, command: expect.objectContaining({ idempotencyKey: b1.headers['Idempotency-Key'] }) }),
        ])
      } finally {
        quota.mockRestore()
        syncPendingCommandOwner(null)
      }
      wrapper.unmount()
    })
  })

  describe('manual publication replay', () => {
    it('replays an unconfirmed publication after a refresh with the same key and restored content', async () => {
      vi.mocked(createEnvironmentTemplateRelease).mockRejectedValueOnce(new TypeError('Failed to fetch'))
      const first = await mountEmpty()
      await fillContainerDraft(first.wrapper)
      await first.wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      first.wrapper.unmount()

      vi.mocked(createEnvironmentTemplateRelease).mockResolvedValue({ data: { operationId: 'op-1', statusUrl: '/x' }, error: undefined } as never)
      const second = await mountEmpty()
      expect(second.wrapper.get('[data-testid="manual-pending"]').text()).toContain('上次发布的结果尚未确认')
      expect((field(second.wrapper, '模板名称').element as HTMLInputElement).value).toBe('Python 分析')
      // The reviewer confirms again before the replay.
      expect((second.wrapper.get('[data-testid="manual-confirm"]').element as HTMLInputElement).checked).toBe(false)
      await second.wrapper.get('[data-testid="manual-confirm"]').setValue(true)
      await second.wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      const [lost, replay] = vi.mocked(createEnvironmentTemplateRelease).mock.calls.map(([options]) => options)
      expect(replay.headers['Idempotency-Key']).toBe(lost.headers['Idempotency-Key'])
      expect(replay.body.manual?.spec.retention.policyId).toBe(lost.body.manual?.spec.retention.policyId)
      expect(second.wrapper.find('[data-testid="manual-pending"]').exists()).toBe(false)
      expect(Object.keys(window.sessionStorage).filter((key) => key.startsWith('labweaver.pending-command:'))).toEqual([])
      second.wrapper.unmount()
    })

    it('keeps the command when the form is collapsed and reopened', async () => {
      vi.mocked(createEnvironmentTemplateRelease).mockResolvedValue({ data: undefined, error: { status: 503, diagnosticCode: 'LW_CONTROL_UNAVAILABLE', detail: 'down', retryable: true } } as never)
      const { wrapper } = await mountEmpty()
      await fillContainerDraft(wrapper)
      await wrapper.get('.manual-form').trigger('submit')
      await flushPromises()

      const section = wrapper.get<HTMLDetailsElement>('[data-testid="manual-section"]')
      section.element.open = false
      await section.trigger('toggle')
      expect(wrapper.find('[data-testid="manual-publisher"]').exists()).toBe(false)
      section.element.open = true
      await section.trigger('toggle')
      await flushPromises()

      expect(wrapper.get('[data-testid="manual-pending"]').exists()).toBe(true)
      await wrapper.get('[data-testid="manual-confirm"]').setValue(true)
      await wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      const keys = publishKeys()
      expect(keys).toHaveLength(2)
      expect(keys[1]).toBe(keys[0])
      wrapper.unmount()
    })

    it('gives changed content a new identity', async () => {
      vi.mocked(createEnvironmentTemplateRelease).mockResolvedValue({ data: undefined, error: { status: 503, diagnosticCode: 'LW_CONTROL_UNAVAILABLE', detail: 'down', retryable: true } } as never)
      const { wrapper } = await mountEmpty()
      await fillContainerDraft(wrapper)
      await wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      await field(wrapper, '模板名称').setValue('Python 分析（第二版）')
      await wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      const [first, second] = vi.mocked(createEnvironmentTemplateRelease).mock.calls.map(([options]) => options)
      expect(second.headers['Idempotency-Key']).not.toBe(first.headers['Idempotency-Key'])
      expect(second.body.manual?.spec.retention.policyId).toBe(manualOptions.retention.policyId)
      wrapper.unmount()
    })

    it('drops the previous actor\'s unconfirmed publication when another user signs in', async () => {
      vi.mocked(createEnvironmentTemplateRelease).mockRejectedValueOnce(new TypeError('Failed to fetch'))
      const first = await mountEmpty()
      await fillContainerDraft(first.wrapper)
      await first.wrapper.get('.manual-form').trigger('submit')
      await flushPromises()
      first.wrapper.unmount()
      expect(Object.keys(window.sessionStorage).some((key) => key.startsWith('labweaver.pending-command:owner-1:'))).toBe(true)

      signIn('admin-2', ['admin'])
      expect(Object.keys(window.sessionStorage).filter((key) => key.startsWith('labweaver.pending-command:'))).toEqual([])
      const second = await mountEmpty()
      expect(second.wrapper.find('[data-testid="manual-pending"]').exists()).toBe(false)
      expect((field(second.wrapper, '模板名称').element as HTMLInputElement).value).toBe('')
      second.wrapper.unmount()
    })
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
