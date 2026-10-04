import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { canonicalRateAmount, equivalentRatePrice, rateVersionState } from '@/utils/resourceRates'
import { navigationGroupsForRoles, navigationTarget } from '@/utils/navigation'
import ResourceFinanceView from '@/views/admin/ResourceFinanceView.vue'
import { createResourceRate, listResourceRates, listResourceGpuCatalog } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    createResourceRate: vi.fn(),
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

async function fillGpuForm(wrapper: ReturnType<typeof mountView>, selection = 'nvidia-v100-2q:vm_vgpu') {
  const form = wrapper.get('[data-testid="resource-rate-form"]')
  await form.get('select[aria-label="GPU 目录分配类型"]').setValue(selection)
  await form.get('input[aria-label="费率单价"]').setValue('0.25')
  await form.get('input[type="datetime-local"]').setValue('2030-01-01T00:00')
  return form
}

describe('ResourceFinanceView', () => {
  afterEach(() => { for (const wrapper of mountedViews.splice(0)) wrapper.unmount() })
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(createResourceRate).mockReset()
    vi.mocked(listResourceRates).mockReset()
    vi.mocked(listResourceGpuCatalog).mockReset()
    vi.mocked(listResourceRates).mockResolvedValue({ data: [] as never, error: undefined as never })
    vi.mocked(listResourceGpuCatalog).mockResolvedValue({ data: gpuCatalog as never, error: undefined as never })
    api.get.mockImplementation(({ url }: { url: string }) => Promise.resolve(
      url.endsWith('/resource-budget') ? { error: 'LW_RESOURCE_BUDGET_NOT_FOUND' } : { data: [] },
    ))
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
