import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { canonicalRateAmount, equivalentRatePrice, rateVersionState } from '@/utils/resourceRates'
import { navigationGroupsForRoles, navigationTarget } from '@/utils/navigation'
import ResourceFinanceView from '@/views/admin/ResourceFinanceView.vue'
import {
  createResourceRate,
  endResourceRate,
  listEnvironments,
  listProjectResourceLeases,
  listProjectResourceRequests,
  listProjectResourceUsage,
  listResourceRates,
  listResourceGpuCatalog,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    createResourceRate: vi.fn(),
    endResourceRate: vi.fn(),
    listEnvironments: vi.fn(),
    listProjectResourceLeases: vi.fn(),
    listProjectResourceRequests: vi.fn(),
    listProjectResourceUsage: vi.fn(),
    listResourceRates: vi.fn(),
    listResourceGpuCatalog: vi.fn(),
  }
})

const api = vi.hoisted(() => ({
  get: vi.fn(),
  put: vi.fn(),
  post: vi.fn(),
}))

interface TestProject {
  id: string
  name: string
  courseId: string | null
}

type ProjectListState =
  | { kind: 'loading'; message: string }
  | { kind: 'success'; data: TestProject[] }

interface TestProjectsState {
  projects: ProjectListState
  selectedProjectId: string | null
  selectedProject: TestProject | null
  select: (id: string) => void
  load: () => void
}

const projectMocks = vi.hoisted(() => ({
  state: null as TestProjectsState | null,
  catalog: [] as TestProject[],
}))

const routerMocks = vi.hoisted(() => ({
  route: { query: {} as Record<string, string | undefined> },
  replace: vi.fn(),
}))

vi.mock('@/api/client', () => ({ apiClient: api }))

vi.mock('@/composables/useProjects', async () => {
  const { reactive } = await import('vue')
  const projectNew = { id: 'project-new', name: '新项目', courseId: null }
  const projectOther = { id: 'project-other', name: '另一个项目', courseId: 'course-2' }
  projectMocks.catalog = [projectNew, projectOther]
  const state = reactive({
    projects: {
      kind: 'success' as const,
      data: projectMocks.catalog,
    } as ProjectListState,
    selectedProjectId: projectNew.id as string | null,
    selectedProject: projectNew as TestProject | null,
    select(id: string) {
      state.selectedProjectId = id
      state.selectedProject = projectMocks.catalog.find((project) => project.id === id) ?? null
    },
    load: vi.fn(),
  })
  projectMocks.state = state
  return { useProjects: () => state }
})

vi.mock('vue-router', async (importOriginal) => {
  const actual = await importOriginal<typeof import('vue-router')>()
  const { reactive } = await import('vue')
  routerMocks.route = reactive(routerMocks.route)
  return {
    ...actual,
    useRoute: () => routerMocks.route,
    useRouter: () => ({ replace: routerMocks.replace }),
  }
})

const mountedViews: Array<{ unmount: () => void }> = []

function mountView() {
  const wrapper = mount(ResourceFinanceView, {
    global: { stubs: { RouterLink: { props: ['to'], template: '<a :data-path="typeof to === \'string\' ? to : to.path"><slot /></a>' } } },
  })
  mountedViews.push(wrapper)
  return wrapper
}

const gpuCatalog = [
  { id: 'gpu-vgpu', class: 'nvidia-v100-2q', mode: 'vm_vgpu', active: true, revision: 1, capacityUnits: 16, providerBinding: 'vm', allocationBinding: 'grid' },
  { id: 'gpu-shared', class: 'nvidia-cuda-shared', mode: 'container_time_slice', active: true, revision: 1, capacityUnits: 8, providerBinding: 'container', allocationBinding: 'gpu' },
  { id: 'gpu-inactive', class: 'retired', mode: 'exclusive', active: false, revision: 1, capacityUnits: 1, providerBinding: 'container', allocationBinding: 'gpu' },
]
const existingRate = { id: 'rate-vgpu', revision: 1, unit: 'gpu_unit_second', unitQuantity: 1,
  gpuClass: 'nvidia-v100-2q', gpuMode: 'vm_vgpu', unitPrice: { currency: 'USD', amount: '0.250000' },
  effectiveFrom: '2026-01-01T00:00:00Z', effectiveUntil: null }
const otherOpenRate = { id: 'rate-memory', revision: 1, unit: 'memory_byte_second', unitQuantity: 1_000_000_000,
  gpuClass: null, gpuMode: null, unitPrice: { currency: 'USD', amount: '0.100000' },
  effectiveFrom: '2026-01-01T00:00:00Z', effectiveUntil: null }
const knownUsage = {
  id: 'usage-compute', projectId: 'project-new', kind: 'compute',
  measuredFrom: '2026-10-06T00:00:00Z', measuredUntil: '2026-10-06T01:00:00Z', observedAt: '2026-10-06T01:01:00Z',
  settlement: 'pending', sourceEventId: 'event-compute',
  measurement: { state: 'known', quantities: { cpuMillicoreSeconds: 3_600_000, gpuUnitSeconds: 120, memoryByteSeconds: 2_147_483_648, storageByteSeconds: 0 } },
  target: { kind: 'experiment_environment', environmentId: 'env-course' },
}
const unknownUsage = {
  id: 'usage-storage', projectId: 'project-new', kind: 'storage',
  measuredFrom: '2026-10-06T01:00:00Z', measuredUntil: '2026-10-06T02:00:00Z', observedAt: '2026-10-06T02:01:00Z',
  settlement: 'unsettled', sourceEventId: 'event-storage',
  measurement: { state: 'unknown', reason: '存储探针尚未上报结束值' },
  target: { kind: 'resource_request', requestId: 'request-work', leaseId: 'lease-work' },
}

async function fillGpuForm(wrapper: ReturnType<typeof mountView>, selection = 'nvidia-v100-2q:vm_vgpu') {
  const form = wrapper.get('[data-testid="resource-rate-form"]')
  await form.get('select[aria-label="GPU 目录分配类型"]').setValue(selection)
  await form.get('input[aria-label="费率单价"]').setValue('0.25')
  await form.get('input[type="datetime-local"]').setValue('2030-01-01T00:00')
  return form
}

async function openEndEditor(wrapper: ReturnType<typeof mountView>, rateId = 'rate-vgpu') {
  await wrapper.get(`[data-testid="end-rate-${rateId}"]`).trigger('click')
  return wrapper.get('[data-testid="resource-rate-end-form"]')
}

async function confirmEnd(wrapper: ReturnType<typeof mountView>, cutoff = '2030-01-01T00:00', rateId = 'rate-vgpu') {
  const editor = await openEndEditor(wrapper, rateId)
  await editor.get('input[aria-label="费率截止时间"]').setValue(cutoff)
  await editor.get('button.filled-button').trigger('click')
  const button = document.body.querySelector<HTMLButtonElement>('.confirm-dialog .filled-button')
  expect(button).not.toBeNull()
  button!.click()
  await flushPromises()
}

describe('ResourceFinanceView', () => {
  afterEach(() => { for (const wrapper of mountedViews.splice(0)) wrapper.unmount() })
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(createResourceRate).mockReset()
    vi.mocked(endResourceRate).mockReset()
    vi.mocked(listEnvironments).mockReset()
    vi.mocked(listProjectResourceLeases).mockReset()
    vi.mocked(listProjectResourceRequests).mockReset()
    vi.mocked(listProjectResourceUsage).mockReset()
    vi.mocked(listResourceRates).mockReset()
    vi.mocked(listResourceGpuCatalog).mockReset()
    vi.mocked(listResourceRates).mockResolvedValue({ data: [] as never, error: undefined as never })
    vi.mocked(listResourceGpuCatalog).mockResolvedValue({ data: gpuCatalog as never, error: undefined as never })
    vi.mocked(listEnvironments).mockResolvedValue({ data: { items: [], nextCursor: null } as never, error: undefined as never })
    vi.mocked(listProjectResourceLeases).mockResolvedValue({ data: [] as never, error: undefined as never })
    vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: [] as never, error: undefined as never })
    api.get.mockImplementation(({ url }: { url: string }) => Promise.resolve(
      url.endsWith('/resource-budget')
        ? { error: 'LW_RESOURCE_BUDGET_NOT_FOUND' }
        : url.endsWith('/usage')
          ? { data: { items: [], page: 1, pageSize: 25, hasMore: false } }
          : { data: [] },
    ))
    vi.mocked(listProjectResourceUsage).mockImplementation(({ path, query }) => api.get({
      url: `/api/v1/projects/${path.projectId}/usage`,
      query,
    }))
    routerMocks.replace.mockImplementation(({ query }: { query: Record<string, string | undefined> }) => { routerMocks.route.query = query })
    routerMocks.route.query = { projectId: 'project-new' }
    projectMocks.state!.projects = { kind: 'success', data: projectMocks.catalog }
    projectMocks.state!.selectedProjectId = 'project-new'
    projectMocks.state!.selectedProject = projectMocks.catalog[0]
  })

  it('keeps global rates usable with no project and does not infer project access from a global list', async () => {
    routerMocks.route.query = {}
    const wrapper = mountView()
    await flushPromises()
    expect(api.get).not.toHaveBeenCalled()
    expect(listResourceRates).toHaveBeenCalledTimes(1)
    expect(wrapper.find('.finance-layout').exists()).toBe(false)
    expect(wrapper.text()).toContain('项目财务未加载')
    const form = await fillGpuForm(wrapper)
    vi.mocked(createResourceRate).mockResolvedValue({ data: existingRate as never, error: undefined as never })
    await form.trigger('submit')
    await flushPromises()
    expect(createResourceRate).toHaveBeenCalledWith(expect.objectContaining({ body: expect.objectContaining({ gpuClass: 'nvidia-v100-2q', gpuMode: 'vm_vgpu', unitQuantity: 1, unitPrice: { amount: '0.250000', currency: 'USD' } }) }))
    expect(api.get).not.toHaveBeenCalled()
  })

  it('shows measured and unknown project usage with human targets and advanced identifiers', async () => {
    const charge = {
      id: 'charge-compute', usageRecordId: knownUsage.id, projectId: 'project-new', courseId: null,
      lines: [{ rateId: 'rate-cpu', rateRevision: 3, unit: 'cpu_millicore_second', quantity: 3_600_000, unitQuantity: 3_600_000,
        unitPrice: { currency: 'USD', amount: '0.010000' }, amount: { currency: 'USD', amount: '0.010000' } }],
      total: { currency: 'USD', amount: '0.010000' }, settlement: 'pending', createdAt: '2026-10-06T01:02:00Z',
      adjustmentOf: null, adjustmentReason: null, adjustedBy: null, diagnosticCode: null,
    }
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      if (url.endsWith('/charges')) return Promise.resolve({ data: [charge] })
      return Promise.resolve({ data: { items: [knownUsage, unknownUsage], page: 1, pageSize: 25, hasMore: false } })
    })
    vi.mocked(listEnvironments).mockResolvedValue({ data: { items: [{ id: 'env-course', displayLabel: '课程实验环境' }] } as never, error: undefined as never })
    vi.mocked(listProjectResourceRequests).mockResolvedValue({ data: [{ id: 'request-work', state: 'active', target: { kind: 'task', taskRunId: 'task-work' } }] as never, error: undefined as never })
    vi.mocked(listProjectResourceLeases).mockResolvedValue({ data: [{ id: 'lease-work', requestId: 'request-work', state: 'active' }] as never, error: undefined as never })

    const wrapper = mountView()
    await flushPromises()

    const usageList = wrapper.get('[aria-label="项目用量列表"]')
    expect(usageList.text()).toContain('教学实验环境：课程实验环境')
    expect(usageList.text()).toContain('CPU 3,600,000 millicore·秒')
    expect(usageList.text()).toContain('GPU 120 单位·秒')
    expect(usageList.text()).toContain('无法确认用量：存储探针尚未上报结束值')
    expect(usageList.text()).toContain('一次性任务资源 · 已分配')
    expect(wrapper.get('.charge-list').text()).toContain('教学实验环境：课程实验环境')
    expect(wrapper.get('.charge-list').text()).toContain('费率版本 3')
    expect(wrapper.findAll('.usage-row details')[0].text()).toContain('usage-compute')
    expect(wrapper.findAll('.charge-row details')[0].text()).toContain('charge-compute')
  })

  it('requests the next usage page and keeps the page boundary visible', async () => {
    const pageOne = { ...knownUsage, id: 'usage-page-1' }
    const pageTwo = { ...knownUsage, id: 'usage-page-2', kind: 'storage' }
    api.get.mockImplementation(({ url, query }: { url: string; query?: { page?: number } }) => {
      if (url.endsWith('/resource-budget')) return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      if (url.endsWith('/usage')) {
        const page = query?.page ?? 1
        return Promise.resolve({ data: { items: [page === 1 ? pageOne : pageTwo], page, pageSize: 1, hasMore: page === 1 } })
      }
      return Promise.resolve({ data: [] })
    })
    const wrapper = mountView()
    await flushPromises()
    expect(wrapper.get('[aria-label="项目用量分页"]').text()).toContain('第 1 页')
    await wrapper.get('[aria-label="项目用量分页"]').findAll('button')[1].trigger('click')
    await flushPromises()
    expect(wrapper.get('[aria-label="项目用量分页"]').text()).toContain('第 2 页')
    expect(listProjectResourceUsage).toHaveBeenLastCalledWith({
      path: { projectId: 'project-new' },
      query: { page: 2, pageSize: 1 },
    })
    expect(wrapper.get('[aria-label="项目用量列表"]').text()).toContain('存储用量')
  })

  it('recovers project usage after a read error without inventing a zero amount', async () => {
    let usageReads = 0
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      if (url.endsWith('/usage') && usageReads++ === 0) return Promise.resolve({ error: { diagnosticCode: 'RESOURCE_USAGE_LOAD_FAILED', detail: '用量服务暂不可用', retryable: true } })
      if (url.endsWith('/usage')) return Promise.resolve({ data: { items: [], page: 1, pageSize: 25, hasMore: false } })
      return Promise.resolve({ data: [] })
    })
    const wrapper = mountView()
    await flushPromises()
    expect(wrapper.get('.usage-card').text()).toContain('用量服务暂不可用')
    await wrapper.get('.usage-card .diagnostic-banner button').trigger('click')
    await flushPromises()
    expect(wrapper.get('.usage-card').text()).toContain('该项目暂无用量记录')
    expect(usageReads).toBe(2)
  })

  it('keeps project names primary and moves the internal project ID to advanced details', async () => {
    const wrapper = mountView()
    await flushPromises()
    expect(wrapper.get('.project-strip option[value="project-new"]').text()).toBe('新项目')
    expect(wrapper.get('.project-id-details').text()).toContain('project-new')
  })

  it('ignores a late usage response after switching project context', async () => {
    const pending = new Map<string, (value: unknown) => void>()
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      if (url.endsWith('/charges')) return Promise.resolve({ data: [] })
      const projectId = url.split('/projects/')[1].split('/')[0]
      return new Promise((resolve) => pending.set(projectId, resolve))
    })
    const wrapper = mountView()
    await wrapper.get('.project-strip select').setValue('project-other')
    await flushPromises()
    pending.get('project-other')!({ data: { items: [{ ...knownUsage, projectId: 'project-other', id: 'usage-other' }], page: 1, pageSize: 25, hasMore: false } })
    await flushPromises()
    expect(wrapper.get('[aria-label="项目用量列表"]').text()).toContain('计算用量')
    pending.get('project-new')!({ data: { items: [{ ...knownUsage, id: 'usage-old' }], page: 1, pageSize: 25, hasMore: false } })
    await flushPromises()
    expect(wrapper.get('[aria-label="项目用量列表"]').text()).toContain('usage-other')
    expect(wrapper.get('[aria-label="项目用量列表"]').text()).not.toContain('usage-old')
  })

  it('creates a shared allocation-unit price from the real catalog without manually typing a class', async () => {
    const wrapper = mountView()
    await flushPromises()
    const form = await fillGpuForm(wrapper, 'nvidia-cuda-shared:container_time_slice')
    expect(form.text()).toContain('共享一张 GPU 的时间片')
    expect(form.findAll('option').some((item) => item.text().includes('retired'))).toBe(false)
    await form.get('input[aria-label="费率单价"]').setValue('0.000100')
    vi.mocked(createResourceRate).mockResolvedValue({ data: existingRate as never, error: undefined as never })
    await form.trigger('submit')
    await flushPromises()
    expect(createResourceRate).toHaveBeenCalledWith({ headers: { 'Idempotency-Key': expect.any(String) }, body: {
      unit: 'gpu_unit_second', unitQuantity: 1, gpuClass: 'nvidia-cuda-shared', gpuMode: 'container_time_slice',
      unitPrice: { currency: 'USD', amount: '0.000100' }, effectiveFrom: new Date('2030-01-01T00:00').toISOString(), effectiveUntil: null,
    } })
  })

  it.each([
    ['cpu_millicore_second', 3_600_000, '核心小时'],
    ['memory_byte_second', 3_865_470_566_400, 'GiB 小时'],
    ['storage_byte_second', 3_865_470_566_400, 'GiB 小时'],
  ])('submits %s in exact standard resource-hour units without floating point conversion', async (unit, quantity, label) => {
    const wrapper = mountView()
    await flushPromises()
    const form = wrapper.get('[data-testid="resource-rate-form"]')
    await form.get('select[aria-label="计费单位"]').setValue(unit)
    await form.get('input[aria-label="费率单价"]').setValue('9007199254740993.123457')
    await form.get('input[type="datetime-local"]').setValue('2030-01-01T00:00')
    expect(form.get('[role="status"]').text()).toContain(`9007199254740993.123457 USD / ${label}`)
    expect(createResourceRate).not.toHaveBeenCalled()
    vi.mocked(createResourceRate).mockResolvedValue({ data: existingRate as never, error: undefined as never })
    await form.trigger('submit')
    await flushPromises()
    expect(createResourceRate).toHaveBeenCalledWith(expect.objectContaining({ body: expect.objectContaining({ unit, unitQuantity: quantity, gpuClass: null, gpuMode: null, unitPrice: { amount: '9007199254740993.123457', currency: 'USD' } }) }))
  })

  it('retries an unknown create response with the original payload and intent key even after editing the draft', async () => {
    vi.mocked(createResourceRate).mockRejectedValueOnce(new Error('request timed out')).mockResolvedValueOnce({ data: existingRate as never, error: undefined as never })
    const wrapper = mountView()
    await flushPromises()
    const form = await fillGpuForm(wrapper)
    await form.trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain('RESOURCE_RATE_CREATE_FAILED')
    const first = createResourceRate.mock.calls[0][0]
    await form.get('input[aria-label="费率单价"]').setValue('7')
    await wrapper.get('.diagnostic-banner button').trigger('click')
    await flushPromises()
    expect(createResourceRate).toHaveBeenCalledTimes(2)
    expect(createResourceRate.mock.calls[1][0]).toEqual(first)
    expect(wrapper.text()).toContain('资源费率已创建')
  })

  it('reports an accepted create with a failed refresh and retries only the list', async () => {
    const wrapper = mountView()
    await flushPromises()
    const form = await fillGpuForm(wrapper)
    vi.mocked(createResourceRate).mockResolvedValue({ data: existingRate as never, error: undefined as never })
    vi.mocked(listResourceRates).mockRejectedValueOnce(new Error('network response unknown'))
    await form.trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain('已创建，但列表刷新失败')
    await wrapper.get('.diagnostic-banner button').trigger('click')
    await flushPromises()
    expect(createResourceRate).toHaveBeenCalledTimes(1)
    expect(listResourceRates).toHaveBeenCalledTimes(3)
  })

  it.each(['', '-1', '1.0000001', 'invalid'])('rejects invalid price %s without rounding or submitting', async (amount) => {
    const wrapper = mountView()
    await flushPromises()
    const form = await fillGpuForm(wrapper)
    await form.get('input[aria-label="费率单价"]').setValue(amount)
    expect(form.get('button[type="submit"]').attributes('disabled')).toBeDefined()
    await form.trigger('submit')
    expect(createResourceRate).not.toHaveBeenCalled()
  })

  it('validates future time, end order and currency before submitting', async () => {
    const wrapper = mountView()
    await flushPromises()
    const form = await fillGpuForm(wrapper)
    const times = form.findAll('input[type="datetime-local"]')
    await times[0].setValue('2020-01-01T00:00')
    expect(form.text()).toContain('须在未来生效')
    await times[0].setValue('2030-01-01T00:00')
    await times[1].setValue('2029-01-01T00:00')
    expect(form.text()).toContain('结束时间须晚于')
    await times[1].setValue('2031-01-01T00:00')
    await form.get('input[maxlength="32"]').setValue('bad currency')
    expect(form.get('button[type="submit"]').attributes('disabled')).toBeDefined()
    await form.trigger('submit')
    expect(createResourceRate).not.toHaveBeenCalled()
    await form.get('input[maxlength="32"]').setValue('EUR')
    vi.mocked(createResourceRate).mockResolvedValue({ data: existingRate as never, error: undefined as never })
    await form.trigger('submit')
    await flushPromises()
    expect(createResourceRate).toHaveBeenCalledWith(expect.objectContaining({ body: expect.objectContaining({ effectiveUntil: new Date('2031-01-01T00:00').toISOString(), unitPrice: { currency: 'EUR', amount: '0.250000' } }) }))
  })

  it('preserves existing price quantities, shows version time states and never copies or changes them', async () => {
    const rates = [
      { ...existingRate, id: 'memory', unit: 'memory_byte_second', unitQuantity: 1_000_000, unitPrice: { currency: 'USD', amount: '1.000000' } },
      { ...existingRate, id: 'future', effectiveFrom: '2030-01-01T00:00:00Z' },
      { ...existingRate, id: 'ended', effectiveUntil: '2020-01-01T00:00:00Z' },
    ]
    vi.mocked(listResourceRates).mockResolvedValue({ data: rates as never, error: undefined as never })
    const wrapper = mountView()
    await flushPromises()
    expect(wrapper.get('.rate-list').text()).toContain('1000000 基础单位 · 1.000000 USD')
    expect(wrapper.get('.rate-list').text()).toContain('3865470.566400 USD / GiB 小时')
    for (const label of ['现行', '未来生效', '已结束']) expect(wrapper.get('.rate-list').text()).toContain(label)
    expect(wrapper.get('input[aria-label="费率单价"]').element).toHaveProperty('value', '')
    expect(createResourceRate).not.toHaveBeenCalled()
  })

  it('offers ending only for open rates and validates a future cutoff', async () => {
    const rates = [
      existingRate,
      { ...existingRate, id: 'scheduled', effectiveUntil: '2030-01-01T00:00:00.000Z' },
      { ...existingRate, id: 'ended', effectiveUntil: '2020-01-01T00:00:00.000Z' },
    ]
    vi.mocked(listResourceRates).mockResolvedValue({ data: rates as never, error: undefined as never })
    const wrapper = mountView()
    await flushPromises()
    expect(wrapper.get('[data-testid="end-rate-rate-vgpu"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="end-rate-scheduled"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="end-rate-ended"]').exists()).toBe(false)
    expect(wrapper.get('.rate-list').text()).toContain('已安排结束')

    const editor = await openEndEditor(wrapper)
    await editor.get('input[aria-label="费率截止时间"]').setValue('2020-01-01T00:00')
    expect(editor.text()).toContain('截止时间须在未来')
    expect(editor.get('button.filled-button').attributes('disabled')).toBeDefined()
    await editor.get('input[aria-label="费率截止时间"]').setValue('2030-01-01T00:00')
    expect(editor.get('button.filled-button').attributes('disabled')).toBeUndefined()
    await editor.get('button.text-button').trigger('click')
  })

  it('ends an open rate with the exact cutoff request and leaves project charges unchanged', async () => {
    const ended = { ...existingRate, effectiveUntil: '2030-01-01T00:00:00.000Z' }
    vi.mocked(listResourceRates)
      .mockResolvedValueOnce({ data: [existingRate] as never, error: undefined as never })
      .mockResolvedValueOnce({ data: [ended] as never, error: undefined as never })
    vi.mocked(endResourceRate).mockResolvedValue({ data: ended as never, error: undefined as never })
    const wrapper = mountView()
    await flushPromises()
    await confirmEnd(wrapper)
    expect(endResourceRate).toHaveBeenCalledWith({
      path: { rateId: 'rate-vgpu' },
      headers: { 'Idempotency-Key': expect.any(String) },
      body: { effectiveUntil: new Date('2030-01-01T00:00').toISOString() },
    })
    expect(endResourceRate.mock.calls[0][0].headers).not.toHaveProperty('If-Match')
    expect(wrapper.text()).toContain('资源费率已安排结束')
    expect(wrapper.find('.charges-card').exists()).toBe(true)
    expect(api.post).not.toHaveBeenCalled()
    expect(api.put).not.toHaveBeenCalled()
  })

  it('does not submit twice when the end request is still pending', async () => {
    let resolveEnd: ((value: unknown) => void) | undefined
    vi.mocked(listResourceRates).mockResolvedValue({ data: [existingRate] as never, error: undefined as never })
    vi.mocked(endResourceRate).mockImplementation(() => new Promise((resolve) => { resolveEnd = resolve }))
    const wrapper = mountView()
    await flushPromises()
    const editor = await openEndEditor(wrapper)
    await editor.get('input[aria-label="费率截止时间"]').setValue('2030-01-01T00:00')
    await editor.get('button.filled-button').trigger('click')
    const button = document.body.querySelector<HTMLButtonElement>('.confirm-dialog .filled-button')
    expect(button).not.toBeNull()
    button!.click()
    button!.click()
    await flushPromises()
    expect(endResourceRate).toHaveBeenCalledTimes(1)
    resolveEnd!({ data: existingRate, error: undefined })
    await flushPromises()
    expect(endResourceRate).toHaveBeenCalledTimes(1)
  })

  it('confirms an uncertain end from the authoritative list without replaying the POST', async () => {
    const ended = { ...existingRate, effectiveUntil: '2030-01-01T00:00:00.000Z' }
    vi.mocked(listResourceRates)
      .mockResolvedValueOnce({ data: [existingRate] as never, error: undefined as never })
      .mockResolvedValueOnce({ data: [ended] as never, error: undefined as never })
    vi.mocked(endResourceRate).mockRejectedValueOnce(new Error('request timed out'))
    const wrapper = mountView()
    await flushPromises()
    await confirmEnd(wrapper)
    expect(endResourceRate).toHaveBeenCalledTimes(1)
    await wrapper.get('.diagnostic-banner button').trigger('click')
    await flushPromises()
    expect(endResourceRate).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).toContain('已确认截止时间')
  })

  it('retries an uncertain open end with the original body and idempotency key', async () => {
    vi.mocked(listResourceRates).mockResolvedValue({ data: [existingRate] as never, error: undefined as never })
    vi.mocked(endResourceRate)
      .mockRejectedValueOnce(new Error('request timed out'))
      .mockResolvedValueOnce({ data: existingRate as never, error: undefined as never })
    const wrapper = mountView()
    await flushPromises()
    await confirmEnd(wrapper)
    const first = endResourceRate.mock.calls[0][0]
    await wrapper.get('.diagnostic-banner button').trigger('click')
    await flushPromises()
    expect(endResourceRate).toHaveBeenCalledTimes(2)
    expect(endResourceRate.mock.calls[1][0]).toEqual(first)
  })

  it('only retries the authoritative read when that read also fails', async () => {
    vi.mocked(listResourceRates)
      .mockResolvedValueOnce({ data: [existingRate] as never, error: undefined as never })
      .mockRejectedValueOnce(new Error('list unavailable'))
    vi.mocked(endResourceRate).mockRejectedValueOnce(new Error('request timed out'))
    const wrapper = mountView()
    await flushPromises()
    await confirmEnd(wrapper)
    await wrapper.get('.diagnostic-banner button').trigger('click')
    await flushPromises()
    expect(endResourceRate).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).toContain('无法确认费率是否已结束')
  })

  it('does not replay an accepted end when list refresh failed', async () => {
    vi.mocked(listResourceRates)
      .mockResolvedValueOnce({ data: [existingRate] as never, error: undefined as never })
      .mockRejectedValueOnce(new Error('refresh unavailable'))
      .mockResolvedValueOnce({ data: [existingRate] as never, error: undefined as never })
    vi.mocked(endResourceRate).mockResolvedValueOnce({ data: existingRate as never, error: undefined as never })
    const wrapper = mountView()
    await flushPromises()
    await confirmEnd(wrapper)
    expect(wrapper.text()).toContain('列表刷新失败')
    await wrapper.get('.diagnostic-banner button').trigger('click')
    await flushPromises()
    expect(endResourceRate).toHaveBeenCalledTimes(1)
    expect(listResourceRates).toHaveBeenCalledTimes(3)
  })

  it('requires an authoritative read before a changed cutoff can follow an uncertain result', async () => {
    vi.mocked(listResourceRates).mockResolvedValue({ data: [existingRate] as never, error: undefined as never })
    vi.mocked(endResourceRate).mockRejectedValueOnce(new Error('request timed out'))
    const wrapper = mountView()
    await flushPromises()
    await confirmEnd(wrapper, '2030-01-01T00:00')
    const editor = await openEndEditor(wrapper)
    await editor.get('input[aria-label="费率截止时间"]').setValue('2031-01-01T00:00')
    await editor.get('button.filled-button').trigger('click')
    const button = document.body.querySelector<HTMLButtonElement>('.confirm-dialog .filled-button')
    expect(button).not.toBeNull()
    button!.click()
    await flushPromises()
    expect(endResourceRate).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).toContain('修改截止时间不会直接再次提交')
  })

  it('preserves an uncertain original intent when another rate is selected', async () => {
    vi.mocked(listResourceRates).mockResolvedValue({ data: [existingRate, otherOpenRate] as never, error: undefined as never })
    vi.mocked(endResourceRate)
      .mockRejectedValueOnce(new Error('request timed out'))
      .mockResolvedValueOnce({ data: existingRate as never, error: undefined as never })
    const wrapper = mountView()
    await flushPromises()
    await confirmEnd(wrapper)
    const first = endResourceRate.mock.calls[0][0]
    await confirmEnd(wrapper, '2030-01-01T00:00', 'rate-memory')
    expect(endResourceRate).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).toContain('GPU nvidia-v100-2q')
    await wrapper.get('.diagnostic-banner button').trigger('click')
    await flushPromises()
    expect(endResourceRate).toHaveBeenCalledTimes(2)
    expect(endResourceRate.mock.calls[1][0]).toEqual(first)
  })

  it('blocks another rate when confirming the original end result fails to read', async () => {
    vi.mocked(listResourceRates)
      .mockResolvedValueOnce({ data: [existingRate, otherOpenRate] as never, error: undefined as never })
      .mockRejectedValueOnce(new Error('rate list unavailable'))
    vi.mocked(endResourceRate).mockRejectedValueOnce(new Error('request timed out'))
    const wrapper = mountView()
    await flushPromises()
    await confirmEnd(wrapper)
    await confirmEnd(wrapper, '2030-01-01T00:00', 'rate-memory')
    expect(endResourceRate).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).toContain('无法确认费率是否已结束')
    expect(wrapper.text()).toContain('GPU nvidia-v100-2q')
  })

  it('clears an already ended original intent before creating a different rate intent', async () => {
    const ended = { ...existingRate, effectiveUntil: '2030-01-01T00:00:00.000Z' }
    vi.mocked(listResourceRates)
      .mockResolvedValueOnce({ data: [existingRate, otherOpenRate] as never, error: undefined as never })
      .mockResolvedValueOnce({ data: [ended, otherOpenRate] as never, error: undefined as never })
      .mockResolvedValueOnce({ data: [ended, otherOpenRate] as never, error: undefined as never })
    vi.mocked(endResourceRate)
      .mockRejectedValueOnce(new Error('request timed out'))
      .mockResolvedValueOnce({ data: otherOpenRate as never, error: undefined as never })
    const wrapper = mountView()
    await flushPromises()
    await confirmEnd(wrapper)
    await confirmEnd(wrapper, '2030-01-01T00:00', 'rate-memory')
    expect(endResourceRate).toHaveBeenCalledTimes(2)
    expect(endResourceRate.mock.calls[1][0].path).toEqual({ rateId: 'rate-memory' })
    expect(endResourceRate.mock.calls[1][0].headers['Idempotency-Key']).not.toBe(endResourceRate.mock.calls[0][0].headers['Idempotency-Key'])
  })

  it('allows a new rate intent after an explicit non-retryable end rejection', async () => {
    vi.mocked(listResourceRates).mockResolvedValue({ data: [existingRate, otherOpenRate] as never, error: undefined as never })
    vi.mocked(endResourceRate)
      .mockResolvedValueOnce({ data: undefined as never, error: { diagnosticCode: 'RATE_END_INVALID', detail: '截止时间无效', retryable: false } as never })
      .mockResolvedValueOnce({ data: otherOpenRate as never, error: undefined as never })
    const wrapper = mountView()
    await flushPromises()
    await confirmEnd(wrapper)
    await confirmEnd(wrapper, '2030-01-01T00:00', 'rate-memory')
    expect(endResourceRate).toHaveBeenCalledTimes(2)
    expect(endResourceRate.mock.calls[1][0].path).toEqual({ rateId: 'rate-memory' })
  })

  it.each(['empty', 'error'])('makes an unavailable catalog %s explicit without inventing classes', async (state) => {
    vi.mocked(listResourceGpuCatalog).mockResolvedValue(state === 'empty'
      ? { data: [] as never, error: undefined as never }
      : { data: undefined as never, error: { detail: '目录暂不可用', diagnosticCode: 'LW_RESOURCE_REQUEST_FAILED', retryable: false } as never })
    const wrapper = mountView()
    await flushPromises()
    expect(wrapper.text()).toContain(state === 'empty' ? '尚无启用的 GPU 目录' : '目录暂不可用')
    expect(wrapper.get('[data-testid="resource-rate-form"] button[type="submit"]').attributes('disabled')).toBeDefined()
    expect(createResourceRate).not.toHaveBeenCalled()
  })

  it('preserves explicit project denial while global rate controls remain available', async () => {
    api.get.mockResolvedValue({ error: { diagnosticCode: 'LW_AUTH_SCOPE_DENIED', detail: '无权访问该项目', retryable: false } })
    const wrapper = mountView()
    await flushPromises()
    expect(wrapper.get('.finance-layout').text()).toContain('LW_AUTH_SCOPE_DENIED')
    expect(wrapper.find('.budget-form').exists()).toBe(false)
    expect(wrapper.find('[data-testid="resource-rate-form"]').exists()).toBe(true)
    expect(routerMocks.route.query.projectId).toBe('project-new')
  })

  it('keeps an unavailable URL project blocked rather than switching to a global-list entry', async () => {
    routerMocks.route.query = { projectId: 'project-missing', courseId: 'course-original' }
    const wrapper = mountView()
    await flushPromises()
    expect(wrapper.text()).toContain('PROJECT_CONTEXT_UNAVAILABLE')
    expect(api.get).not.toHaveBeenCalled()
    await wrapper.get('.project-strip select').setValue('project-other')
    await flushPromises()
    expect(api.get).toHaveBeenCalledWith(expect.objectContaining({ url: '/api/v1/projects/project-other/resource-budget' }))
    expect(routerMocks.route.query).toMatchObject({ projectId: 'project-other', courseId: 'course-original' })
    await wrapper.get('.project-strip select').setValue('')
    await flushPromises()
    expect(wrapper.find('.finance-layout').exists()).toBe(false)
  })

  it('keeps budget creation for an explicitly selected project and shows unrelated failures', async () => {
    const wrapper = mountView()
    await flushPromises()
    expect(wrapper.get('.budget-form button[type="submit"]').text()).toBe('创建预算')
    api.get.mockResolvedValue({ error: 'LW_RESOURCE_PROJECT_NOT_FOUND' })
    await wrapper.get('.project-strip select').setValue('project-other')
    await flushPromises()
    expect(wrapper.find('.budget-form').exists()).toBe(false)
    expect(wrapper.get('.budget-card').text()).toContain('RESOURCE_BUDGET_LOAD_FAILED')
  })

  it.each([
    { spent: '0.099999', warningAt: '0.100000', limit: '1.000000', threshold: false, limitReached: false },
    { spent: '0.100000', warningAt: '0.100000', limit: '1.000000', threshold: true, limitReached: false },
    { spent: '1.000000', warningAt: '0.100000', limit: '1.000000', threshold: true, limitReached: true },
  ])('shows budget notices at exact six-decimal boundaries (%s)', async ({ spent, warningAt, limit, threshold, limitReached }) => {
    api.get.mockImplementation(({ url }: { url: string }) => Promise.resolve(
      url.endsWith('/resource-budget')
        ? {
            data: {
              id: 'budget-new', projectId: 'project-new', courseId: null,
              limit: { currency: 'USD', amount: limit },
              warningAt: { currency: 'USD', amount: warningAt },
              spent: { currency: 'USD', amount: spent },
              revision: 1, updatedAt: '2026-10-06T03:00:00Z',
            },
          }
        : url.endsWith('/charges')
          ? { data: [] }
          : { data: { items: [], page: 1, pageSize: 25, hasMore: false } },
    ))
    const wrapper = mountView()
    await flushPromises()
    expect(wrapper.find('[data-testid="budget-threshold-warning"]').exists()).toBe(threshold)
    expect(wrapper.find('[data-testid="budget-limit-warning"]').exists()).toBe(limitReached)
    if (threshold) expect(wrapper.get('[data-testid="budget-threshold-warning"]').text()).toContain(`${spent} USD`)
    if (limitReached) {
      const notice = wrapper.get('[data-testid="budget-limit-warning"]').text()
      expect(notice).toContain(`${limit} USD`)
      expect(notice).toContain('不会自动停止已批准的计划或资源')
    }
  })

  it('offers rate and catalog navigation only to actual platform admin roles', () => {
    const admin = navigationGroupsForRoles(['admin']).flatMap((group) => group.items)
    const finance = admin.find((item) => item.id === 'admin-finance')!
    expect(navigationTarget(finance, null)).toBe('/admin/resource-finance')
    expect(admin.some((item) => item.id === 'admin-gpu-catalog')).toBe(true)
    for (const role of ['teacher', 'student'] as const) expect(navigationGroupsForRoles([role]).flatMap((group) => group.items).some((item) => item.id === 'admin-finance')).toBe(false)
  })
})

describe('exact resource price display', () => {
  it.each([['0.000001', '0.000000'], ['0.000003', '0.000002'], ['0.000005', '0.000002']])('rounds %s for display only using nearest even', (amount, expected) => {
    expect(equivalentRatePrice('gpu_unit_second', 2, amount, 'USD')).toContain(`${expected} USD`)
    expect(canonicalRateAmount(amount)).toBe(amount)
  })
  it.each([0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1])('does not pretend unknown or unsafe quantity %s is zero', (quantity) => {
    expect(equivalentRatePrice('memory_byte_second', quantity, '1', 'USD')).toBeNull()
  })
  it('pads short amounts exactly and rejects malformed rate times', () => {
    expect(canonicalRateAmount('1.2')).toBe('1.200000')
    expect(canonicalRateAmount('0')).toBe('0.000000')
    expect(rateVersionState({ ...existingRate, effectiveFrom: 'invalid' } as never, Date.now())).toBe('时间无效')
  })
})
