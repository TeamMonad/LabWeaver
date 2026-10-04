import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import ResourceFinanceView from '@/views/admin/ResourceFinanceView.vue'
import { createResourceRate, listResourceRates } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    createResourceRate: vi.fn(),
    listResourceRates: vi.fn(),
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

describe('ResourceFinanceView', () => {
  afterEach(() => {
    for (const wrapper of mountedViews.splice(0)) wrapper.unmount()
  })

  beforeEach(() => {
    api.get.mockReset()
    vi.mocked(listResourceRates).mockReset()
    vi.mocked(createResourceRate).mockReset()
    vi.mocked(listResourceRates).mockResolvedValue({ data: [] as never, error: undefined as never })
    routerMocks.replace.mockReset()
    routerMocks.replace.mockImplementation(({ query }: { query: Record<string, string | undefined> }) => {
      routerMocks.route.query = query
    })
    routerMocks.route.query = {}
    projectMocks.state!.projects = { kind: 'success', data: projectMocks.catalog }
    projectMocks.state!.selectedProjectId = 'project-new'
    projectMocks.state!.selectedProject = projectMocks.catalog[0]
  })

  it('shows the editable create form when Resource reports a missing project budget', async () => {
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) {
        // Resource uses this stable plain-text 404 to indicate an unconfigured budget.
        return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      }
      return Promise.resolve({ data: [] })
    })

    const wrapper = mountView()
    await flushPromises()

    expect(wrapper.find('.budget-form').exists()).toBe(true)
    expect(wrapper.find('.budget-form button[type="submit"]').text()).toBe('创建预算')
    expect(wrapper.find('.budget-card .diagnostic-banner').exists()).toBe(false)
  })

  it('lets an administrator create a GPU rate version from the finance page', async () => {
    const rate = {
      id: 'rate-vgpu-1',
      revision: 1,
      unit: 'gpu_unit_second',
      unitQuantity: 1,
      gpuClass: 'nvidia-v100-2q',
      gpuMode: 'vm_vgpu',
      unitPrice: { currency: 'USD', amount: '0.250000' },
      effectiveFrom: '2026-09-08T00:00:00.000Z',
      effectiveUntil: null,
    }
    vi.mocked(listResourceRates)
      .mockResolvedValueOnce({ data: [] as never, error: undefined as never })
      .mockResolvedValue({ data: [rate] as never, error: undefined as never })
    vi.mocked(createResourceRate).mockResolvedValue({ data: rate as never, error: undefined as never })
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      return Promise.resolve({ data: [] })
    })

    const wrapper = mountView()
    await flushPromises()
    const form = wrapper.get('[data-testid="resource-rate-form"]')
    await form.findAll('select')[1].setValue('vm_vgpu')
    await form.find('input').setValue('nvidia-v100-2q')
    const inputs = form.findAll('input')
    await inputs[1].setValue('1')
    await inputs[2].setValue('0.250000')
    await form.trigger('submit')
    await flushPromises()

    expect(createResourceRate).toHaveBeenCalledWith({
      headers: { 'Idempotency-Key': expect.any(String) },
      body: {
        unit: 'gpu_unit_second',
        unitQuantity: 1,
        gpuClass: 'nvidia-v100-2q',
        gpuMode: 'vm_vgpu',
        unitPrice: { currency: 'USD', amount: '0.250000' },
        effectiveFrom: expect.stringMatching(/Z$/),
        effectiveUntil: null,
      },
    })
    expect(wrapper.text()).toContain('GPU nvidia-v100-2q · VM vGPU')
    expect(wrapper.text()).toContain('资源费率已创建。')
  })

  it('reuses the rate create intent key after a transport failure', async () => {
    const rate = {
      id: 'rate-vgpu-1',
      revision: 1,
      unit: 'gpu_unit_second',
      unitQuantity: 1,
      gpuClass: 'nvidia-v100-2q',
      gpuMode: 'vm_vgpu',
      unitPrice: { currency: 'USD', amount: '0.250000' },
      effectiveFrom: '2026-09-08T00:00:00.000Z',
      effectiveUntil: null,
    }
    vi.mocked(createResourceRate)
      .mockRejectedValueOnce(new Error('request timed out'))
      .mockResolvedValueOnce({ data: rate as never, error: undefined as never })
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      return Promise.resolve({ data: [] })
    })

    const wrapper = mountView()
    await flushPromises()
    const form = wrapper.get('[data-testid="resource-rate-form"]')
    await form.findAll('select')[1].setValue('vm_vgpu')
    await form.find('input').setValue('nvidia-v100-2q')
    const inputs = form.findAll('input')
    await inputs[1].setValue('1')
    await inputs[2].setValue('0.250000')

    await form.trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain('RESOURCE_RATE_CREATE_FAILED')

    await form.trigger('submit')
    await flushPromises()

    expect(createResourceRate).toHaveBeenCalledTimes(2)
    expect(createResourceRate.mock.calls[0][0].headers?.['Idempotency-Key']).toBe(
      createResourceRate.mock.calls[1][0].headers?.['Idempotency-Key'],
    )
    expect(wrapper.text()).toContain('资源费率已创建。')
  })

  async function mountRateForm() {
    api.get.mockImplementation(({ url }: { url: string }) => Promise.resolve(
      url.endsWith('/resource-budget') ? { error: 'LW_RESOURCE_BUDGET_NOT_FOUND' } : { data: [] },
    ))
    const wrapper = mountView()
    await flushPromises()
    return { wrapper, form: wrapper.get('[data-testid="resource-rate-form"]') }
  }

  it.each([
    ['cpu_millicore_second', 3_600_000, '核心小时', 'CPU millicore 秒'],
    ['memory_byte_second', 3_865_470_566_400, 'GiB 小时', '内存字节秒'],
    ['storage_byte_second', 3_865_470_566_400, 'GiB 小时', '存储字节秒'],
  ])('previews %s in resource-hours while submitting the selected canonical quantity', async (unit, quantity, label, base) => {
    const { wrapper, form } = await mountRateForm()
    await form.get('select[aria-label="计费单位"]').setValue(unit)
    await form.get('input[type="number"]').setValue(String(quantity))
    await form.get('input[inputmode="decimal"]').setValue('1.000000')
    await form.get('input[type="datetime-local"]').setValue('2030-01-01T00:00')
    expect(form.get('[role="status"]').text()).toContain(`1.000000 USD / ${label}`)
    expect(form.get('[role="status"]').text()).toContain(String(base))
    expect(createResourceRate).not.toHaveBeenCalled()

    vi.mocked(createResourceRate).mockResolvedValue({ data: {} as never, error: undefined as never })
    await form.trigger('submit')
    await flushPromises()
    expect(createResourceRate).toHaveBeenCalledWith({
      headers: { 'Idempotency-Key': expect.any(String) },
      body: {
        unit, unitQuantity: quantity, gpuClass: null, gpuMode: null,
        unitPrice: { amount: '1.000000', currency: 'USD' },
        effectiveFrom: new Date('2030-01-01T00:00').toISOString(), effectiveUntil: null,
      },
    })
    expect(wrapper.text()).toContain('资源费率已创建。')
  })

  it('shows the true GiB-hour equivalent of an existing byte-second rate without rewriting it', async () => {
    const existing = { id: 'memory-rate', revision: 1, unit: 'memory_byte_second', unitQuantity: 1_000_000,
      unitPrice: { amount: '1.000000', currency: 'USD' }, effectiveFrom: '2026-09-08T00:00:00Z' }
    vi.mocked(listResourceRates).mockResolvedValue({ data: [existing] as never, error: undefined as never })
    const { wrapper } = await mountRateForm()
    expect(wrapper.get('.rate-list').text()).toContain('1000000 基础单位 · 1.000000 USD')
    expect(wrapper.get('.rate-list').text()).toContain('3865470.566400 USD / GiB 小时')
    expect(createResourceRate).not.toHaveBeenCalled()
  })

  it.each([
    ['0.000001', '0.000000'],
    ['0.000003', '0.000002'],
    ['0.000005', '0.000002'],
  ])('rounds a half-unit %s price to nearest even at six decimals', async (amount, expected) => {
    const { form } = await mountRateForm()
    await form.get('input[type="number"]').setValue('2')
    await form.get('input[inputmode="decimal"]').setValue(amount)
    expect(form.get('[role="status"]').text()).toContain(`等价单价约：${expected} USD / GPU 单位秒`)
  })

  it('converts a large precise amount without passing it through floating point', async () => {
    const { form } = await mountRateForm()
    await form.get('select[aria-label="计费单位"]').setValue('cpu_millicore_second')
    await form.get('input[type="number"]').setValue('3600000')
    await form.get('input[inputmode="decimal"]').setValue('9007199254740993.123457')
    expect(form.get('[role="status"]').text()).toContain('9007199254740993.123457 USD / 核心小时')
  })

  it.each(['0', '-1', '1.5', '9007199254740992'])('does not submit or replace an invalid quantity %s with zero', async (quantity) => {
    const { form } = await mountRateForm()
    await form.get('input[type="number"]').setValue(quantity)
    await form.get('input[inputmode="decimal"]').setValue('1.000000')
    expect(form.get('[role="status"]').text()).toContain('有效的六位小数金额和安全整数')
    expect(form.get('button[type="submit"]').attributes('disabled')).toBeDefined()
    await form.trigger('submit')
    expect(createResourceRate).not.toHaveBeenCalled()
  })

  it.each(['1.2', 'invalid', '-1.000000'])('leaves a malformed amount %s visibly unconverted', async (amount) => {
    const { form } = await mountRateForm()
    await form.get('input[inputmode="decimal"]').setValue(amount)
    expect(form.get('[role="status"]').text()).toContain('有效的六位小数金额和安全整数')
    expect(form.get('button[type="submit"]').attributes('disabled')).toBeDefined()
  })

  it('keeps an unrelated 404 as a budget load error', async () => {
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) {
        return Promise.resolve({ error: 'LW_RESOURCE_PROJECT_NOT_FOUND', status: 404 })
      }
      return Promise.resolve({ data: [] })
    })

    const wrapper = mountView()
    await flushPromises()

    expect(wrapper.find('.budget-form').exists()).toBe(false)
    expect(wrapper.find('.budget-card .diagnostic-banner').text()).toContain('RESOURCE_BUDGET_LOAD_FAILED')
  })

  it('uses the URL project and resets budget and adjustment drafts when the shared project changes', async () => {
    routerMocks.route.query = { projectId: 'project-other' }
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      return Promise.resolve({ data: [] })
    })

    const wrapper = mountView()
    await flushPromises()

    const projectSelect = wrapper.get('select')
    expect((projectSelect.element as HTMLSelectElement).value).toBe('project-other')
    expect(api.get).toHaveBeenCalledWith(expect.objectContaining({ url: '/api/v1/projects/project-other/resource-budget' }))

    const limitInput = wrapper.get('.budget-form input[inputmode="decimal"]')
    await limitInput.setValue('12.000000')
    await projectSelect.setValue('project-new')
    await flushPromises()

    expect(projectMocks.state!.selectedProjectId).toBe('project-new')
    expect((wrapper.get('.budget-form input[inputmode="decimal"]').element as HTMLInputElement).value).toBe('0.000000')
    expect(routerMocks.replace).toHaveBeenCalledWith({ query: { projectId: 'project-new' } })
  })

  it('blocks an unavailable URL project instead of silently switching to another project', async () => {
    routerMocks.route.query = { projectId: 'project-missing' }
    api.get.mockResolvedValue({ data: [] })

    const wrapper = mountView()
    await flushPromises()

    expect(wrapper.text()).toContain('PROJECT_CONTEXT_UNAVAILABLE')
    expect(wrapper.text()).toContain('不存在或你无权访问')
    expect(api.get).not.toHaveBeenCalled()
    expect(wrapper.get('a[data-path="/researcher/workspaces"]').exists()).toBe(true)

    await wrapper.get('select').setValue('project-new')
    await flushPromises()

    expect(wrapper.text()).not.toContain('PROJECT_CONTEXT_UNAVAILABLE')
    expect(routerMocks.route.query).toMatchObject({ projectId: 'project-new' })
  })

  it('keeps an unavailable URL blocked when the project list resolves and preserves its first selection', async () => {
    routerMocks.route.query = { projectId: 'project-missing' }
    const state = projectMocks.state!
    state.projects = { kind: 'loading', message: '加载项目…' }
    state.selectedProjectId = null
    state.selectedProject = null
    api.get.mockResolvedValue({ data: [] })

    const wrapper = mountView()
    expect(api.get).not.toHaveBeenCalled()

    state.projects = { kind: 'success', data: projectMocks.catalog }
    state.selectedProjectId = 'project-new'
    state.selectedProject = projectMocks.catalog[0]
    await flushPromises()

    expect(state.selectedProjectId).toBe('project-new')
    expect(routerMocks.replace).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('PROJECT_CONTEXT_UNAVAILABLE')
    expect(api.get).not.toHaveBeenCalled()
  })

  it('selects the first project and loads it after the project list finishes loading', async () => {
    const state = projectMocks.state!
    state.projects = { kind: 'loading', message: '加载项目…' }
    state.selectedProjectId = null
    state.selectedProject = null
    api.get.mockImplementation(({ url }: { url: string }) => {
      if (url.endsWith('/resource-budget')) return Promise.resolve({ error: 'LW_RESOURCE_BUDGET_NOT_FOUND' })
      return Promise.resolve({ data: [] })
    })

    const wrapper = mountView()
    expect(api.get).not.toHaveBeenCalled()

    state.projects = { kind: 'success', data: projectMocks.catalog }
    await flushPromises()

    expect(state.selectedProjectId).toBe('project-new')
    expect(api.get).toHaveBeenCalledWith(expect.objectContaining({ url: '/api/v1/projects/project-new/resource-budget' }))
    expect(routerMocks.route.query).toMatchObject({ projectId: 'project-new' })
    expect(wrapper.get('select').element).toHaveProperty('value', 'project-new')
  })
})
