import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import ResourceRequestView from '@/views/researcher/ResourceRequestView.vue'
import {
  createCreditAdminOperation,
  estimateProjectResourceCost,
  getProjectCredits,
  listProjectCreditHolds,
  listProjectCreditLedger,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    createCreditAdminOperation: vi.fn(),
    estimateProjectResourceCost: vi.fn(),
    getProjectCredits: vi.fn(),
    listProjectCreditHolds: vi.fn(),
    listProjectCreditLedger: vi.fn(),
  }
})

const creditAccount = {
  projectId: 'project-1', currency: 'CREDITS', balance: '20.000000', available: '15.000000', reserved: '5.000000',
  spent: '3.000000', pending: '5.000000', unknown: '0.000000', unknownCount: 0, platformAbsorbed: '0.000000', revision: 2,
}
const grantEntry = {
  id: 'entry-grant', kind: 'grant', operationId: 'op-1', reason: '课程额度', balanceDelta: '20.000000', reservedDelta: '0.000000',
  spentDelta: '0.000000', platformAbsorbed: '0.000000', createdAt: '2026-10-12T08:00:00.000Z', rateSnapshots: [],
}

const mocks = vi.hoisted(() => ({
  requests: { kind: 'success', data: [] as unknown[] } as unknown,
  leases: { kind: 'success', data: [] as unknown[] } as unknown,
  releases: { kind: 'success', data: [] as unknown[] } as unknown,
  environments: { kind: 'empty' } as unknown,
  catalog: { kind: 'empty' } as unknown,
  rates: { kind: 'empty' } as unknown,
  cancel: vi.fn(),
  reclaim: vi.fn(),
  renew: vi.fn(),
  load: vi.fn(),
  create: vi.fn(),
  gpuRateSelection: vi.fn(() => ({ rate: null, ambiguous: false })),
}))

const authState = vi.hoisted(() => ({
  user: { value: null as { profile?: unknown } | null },
}))

const projectOne = {
  id: 'project-1',
  name: '课程项目',
  description: null,
  ownerActorId: 'teacher-1',
  courseId: 'course-1',
  state: 'active',
  revision: 1,
  createdAt: '2026-09-01T00:00:00.000Z',
  updatedAt: '2026-09-01T00:00:00.000Z',
}
const projectTwo = { ...projectOne, id: 'project-2', name: '另一个项目', courseId: null }

const projectsState = reactive({
  projects: { kind: 'success' as const, data: [projectOne, projectTwo] },
  selectedProjectId: projectOne.id as string | null,
  selectedProject: projectOne as typeof projectOne | null,
  select(id: string) {
    projectsState.selectedProjectId = id
    projectsState.selectedProject = projectsState.projects.data.find((project) => project.id === id) ?? null
  },
})

vi.mock('@/composables/useProjects', () => ({
  useProjects: () => projectsState,
}))

vi.mock('@/composables/useAuth', () => ({
  useAuth: () => authState,
}))

vi.mock('@/composables/useProjectResources', async () => {
  const { reactive } = await import('vue')
  return {
    resourceSummary: vi.fn(() => '资源摘要'),
    useProjectResources: () => reactive({
      requests: mocks.requests,
      leases: mocks.leases,
      acting: null,
      outcome: null,
      load: mocks.load,
      create: mocks.create,
      cancel: mocks.cancel,
      renew: mocks.renew,
      reclaim: mocks.reclaim,
    }),
  }
})

vi.mock('@/composables/useProjectResourceOptions', async () => {
  const { reactive } = await import('vue')
  return {
    useProjectResourceOptions: () => reactive({
      environments: mocks.environments,
      releases: mocks.releases,
      catalog: mocks.catalog,
      rates: mocks.rates,
      gpuRateSelection: mocks.gpuRateSelection,
      load: mocks.load,
    }),
  }
})

function pendingRequest() {
  return {
    id: 'request-1',
    requestKey: 'work-request-1',
    state: 'reviewing',
    target: { kind: 'environment', environmentId: 'environment-1' },
    requestedResources: { cpuMillicores: 1000, memoryBytes: 1024, storageBytes: 2048 },
    updatedAt: '2026-09-14T00:00:00.000Z',
  }
}

function activeLease() {
  return {
    id: 'lease-1',
    requestId: 'request-1',
    state: 'active',
    expiresAt: '2026-09-15T00:00:00.000Z',
  }
}

async function mountView(projectId = 'project-1', extraQuery: Record<string, string> = {}) {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/researcher/resources', component: ResourceRequestView }],
  })
  await router.push({ path: '/researcher/resources', query: { projectId, ...extraQuery } })
  await router.isReady()
  const wrapper = mount(ResourceRequestView, { global: { plugins: [router] } })
  await flushPromises()
  return wrapper
}

describe('ResourceRequestView', () => {
  beforeEach(() => {
    mocks.requests = { kind: 'success', data: [] }
    mocks.leases = { kind: 'success', data: [] }
    mocks.releases = { kind: 'success', data: [] }
    mocks.environments = { kind: 'empty' }
    mocks.catalog = { kind: 'empty' }
    mocks.rates = { kind: 'empty' }
    mocks.cancel.mockReset()
    mocks.reclaim.mockReset()
    mocks.renew.mockReset()
    mocks.load.mockReset()
    mocks.create.mockReset()
    mocks.create.mockResolvedValue(true)
    mocks.gpuRateSelection.mockReset()
    mocks.gpuRateSelection.mockReturnValue({ rate: null, ambiguous: false })
    authState.user.value = null
    vi.mocked(getProjectCredits).mockReset().mockResolvedValue({ data: creditAccount, error: undefined } as never)
    vi.mocked(listProjectCreditLedger).mockReset().mockResolvedValue({ data: { items: [grantEntry], nextCursor: null }, error: undefined } as never)
    vi.mocked(listProjectCreditHolds).mockReset().mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined } as never)
    vi.mocked(createCreditAdminOperation).mockReset()
    vi.mocked(estimateProjectResourceCost).mockReset()
    projectsState.selectedProjectId = projectOne.id
    projectsState.selectedProject = projectOne
  })

  it('explains a blocked allocation, hides its invalid connection and retains confirmed reclaim', async () => {
    mocks.requests = { kind: 'success', data: [{ ...pendingRequest(), state: 'active', diagnosticCode: 'LW_RESOURCE_WORK_ALLOCATION_BLOCKED' }] }
    mocks.leases = { kind: 'success', data: [activeLease()] }
    const wrapper = await mountView()
    expect(wrapper.text()).toContain('分配失败，请回收后重新申请。')
    expect(wrapper.find('a[href^="/researcher/environments"]').exists()).toBe(false)
    const reclaim = wrapper.findAll('button').find((button) => button.text() === '回收')!
    await reclaim.trigger('click')
    await flushPromises()
    expect(mocks.reclaim).not.toHaveBeenCalled()
    document.body.querySelector<HTMLButtonElement>('[role="alertdialog"] .filled-button')?.click()
    await flushPromises()
    expect(mocks.reclaim).toHaveBeenCalledWith(expect.objectContaining({ id: 'lease-1' }), 'researcher requested Work resource reclaim')
    wrapper.unmount()
  })

  it('retains the normal connection for an active allocation without a failure diagnostic', async () => {
    mocks.requests = { kind: 'success', data: [{ ...pendingRequest(), state: 'active' }] }
    mocks.leases = { kind: 'success', data: [activeLease()] }
    const wrapper = await mountView()
    expect(wrapper.get('a[href^="/researcher/environments"]').attributes('href')).toContain('projectId=project-1')
    wrapper.unmount()
  })

  it('explains task-owner cleanup without exposing an internal reason code as a failure', async () => {
    mocks.requests = {
      kind: 'success',
      data: [{ ...pendingRequest(), target: { kind: 'task', taskRunId: 'task-run-1' }, state: 'active' }],
    }
    mocks.leases = { kind: 'success', data: [{ ...activeLease(), state: 'revoked', revokeReasonCode: 'task_owner_release' }] }
    const wrapper = await mountView()

    expect(wrapper.text()).toContain('临时任务资源已回收，请查看任务结果')
    expect(wrapper.text()).toContain('这不代表用户 Work 环境已释放')
    expect(wrapper.text()).not.toContain('task_owner_release')
    wrapper.unmount()
  })

  it('explains user units and gives an honest empty-release next step', async () => {
    mocks.releases = { kind: 'empty' }
    const wrapper = await mountView()

    expect(wrapper.text()).toContain('CPU（m）')
    expect(wrapper.text()).toContain('1000m = 1 核心')
    expect(wrapper.text()).toContain('当前项目没有可用的已发布 Work 模板')
    expect(wrapper.find('a[href^="/researcher/software"]').exists()).toBe(true)
    expect(wrapper.find('a[href^="/researcher/environments"]').text()).toBe('发布手工 Work 模板')
    expect(wrapper.find('a[href^="/teacher/materials"]').exists()).toBe(false)
  })

  it('keeps the teacher-only publication link hidden for an admin without teacher role', async () => {
    authState.user.value = { profile: { roles: ['admin'] } }
    mocks.releases = { kind: 'empty' }
    const wrapper = await mountView()

    expect(wrapper.find('a[href^="/teacher/materials"]').exists()).toBe(false)
  })

  it('preselects the exact template version chosen on the Work page', async () => {
    mocks.releases = {
      kind: 'success',
      data: [
        { id: 'release-container', version: 1, runtimeKind: 'container', label: '容器模板 v1', source: 'control' },
        { id: 'release-vm', version: 2, runtimeKind: 'virtual_machine', label: '虚拟机模板 v2', source: 'control' },
      ],
    }
    const wrapper = await mountView('project-1', { releaseId: 'release-vm', releaseVersion: '2' })
    const releaseSelect = wrapper.findAll('label').find((label) => label.text().includes('已发布版本'))!.get('select')
    expect((releaseSelect.element as HTMLSelectElement).value).toBe('release-vm:2')
    wrapper.unmount()

    // A version that is no longer offered falls back to the first option instead of a stale one.
    const fallback = await mountView('project-1', { releaseId: 'release-vm', releaseVersion: '1' })
    const fallbackSelect = fallback.findAll('label').find((label) => label.text().includes('已发布版本'))!.get('select')
    expect((fallbackSelect.element as HTMLSelectElement).value).toBe('release-container:1')
    fallback.unmount()
  })

  it('only offers GPU modes supported by the selected release runtime without requiring a rate', async () => {
    mocks.releases = {
      kind: 'success',
      data: [
        { id: 'release-container', version: 1, runtimeKind: 'container', label: 'Container release', source: 'control' },
        { id: 'release-vm', version: 1, runtimeKind: 'virtual_machine', label: 'VM release', source: 'control' },
      ],
    }
    mocks.catalog = {
      kind: 'success',
      data: [
        { id: 'gpu-container', class: 'nvidia-cuda', mode: 'exclusive', capacityUnits: 1, revision: 1, active: true },
        { id: 'gpu-vm', class: 'nvidia-v100-2q', mode: 'vm_vgpu', capacityUnits: 16, revision: 1, active: true },
      ],
    }
    const wrapper = await mountView()
    const releaseSelect = wrapper.findAll('label').find((label) => label.text().includes('已发布版本'))?.get('select')
    const gpuSelect = wrapper.findAll('label').find((label) => label.text().includes('GPU 目录项'))?.get('select')
    expect(releaseSelect).toBeDefined()
    expect(gpuSelect).toBeDefined()
    expect(gpuSelect!.findAll('option').map((option) => option.element.value)).toEqual(['', 'gpu-container'])

    await releaseSelect!.setValue('release-vm:1')
    await flushPromises()
    expect(gpuSelect!.findAll('option').map((option) => option.element.value)).toEqual(['', 'gpu-vm'])
    await gpuSelect!.setValue('gpu-vm')
    expect((wrapper.get('form.request-form button[type="submit"]').element as HTMLButtonElement).disabled).toBe(false)

    await releaseSelect!.setValue('release-container:1')
    await flushPromises()
    expect((gpuSelect!.element as HTMLSelectElement).value).toBe('gpu-vm')
    expect(wrapper.text()).toContain('先前选择的 GPU 目录项当前不可用')
    expect((wrapper.get('form.request-form button[type="submit"]').element as HTMLButtonElement).disabled).toBe(true)
  })

  it('submits without a GPU rate and keeps invalid resource input blocked', async () => {
    mocks.releases = {
      kind: 'success',
      data: [{ id: 'release-container', version: 1, runtimeKind: 'container', label: 'Container release', source: 'control' }],
    }
    mocks.catalog = {
      kind: 'success',
      data: [{ id: 'gpu-container', class: 'nvidia-cuda', mode: 'exclusive', capacityUnits: 1, revision: 1, active: true }],
    }
    const wrapper = await mountView()
    const gpuSelect = wrapper.findAll('label').find((label) => label.text().includes('GPU 目录项'))?.get('select')
    await gpuSelect!.setValue('gpu-container')
    const submit = wrapper.get('form.request-form button[type="submit"]')

    expect(wrapper.text()).toContain('当前未配置计价')
    expect((submit.element as HTMLButtonElement).disabled).toBe(false)
    await wrapper.get('form.request-form').trigger('submit')
    await flushPromises()
    expect(mocks.create).toHaveBeenCalledTimes(1)

    const cpuInput = wrapper.findAll('label').find((label) => label.text().includes('CPU（m）'))!.get('input')
    await cpuInput.setValue('0')
    expect((submit.element as HTMLButtonElement).disabled).toBe(true)
    wrapper.unmount()
  })

  it('submits with an ambiguous or unreadable GPU rate without presenting it as free', async () => {
    mocks.releases = {
      kind: 'success',
      data: [{ id: 'release-container', version: 1, runtimeKind: 'container', label: 'Container release', source: 'control' }],
    }
    mocks.catalog = {
      kind: 'success',
      data: [{ id: 'gpu-container', class: 'nvidia-cuda', mode: 'exclusive', capacityUnits: 1, revision: 1, active: true }],
    }
    mocks.rates = { kind: 'success', data: [] }
    mocks.gpuRateSelection.mockReturnValue({ rate: null, ambiguous: true })
    const ambiguousWrapper = await mountView()
    const ambiguousGpuSelect = ambiguousWrapper.findAll('label').find((label) => label.text().includes('GPU 目录项'))!.get('select')
    await ambiguousGpuSelect.setValue('gpu-container')
    const ambiguousSubmit = ambiguousWrapper.get('form.request-form button[type="submit"]')
    expect(ambiguousWrapper.text()).toContain('当前有效费率存在冲突')
    expect(ambiguousWrapper.text()).not.toContain('费用：免费')
    expect((ambiguousSubmit.element as HTMLButtonElement).disabled).toBe(false)
    await ambiguousWrapper.get('form.request-form').trigger('submit')
    await flushPromises()
    expect(mocks.create).toHaveBeenCalledTimes(1)
    ambiguousWrapper.unmount()

    mocks.create.mockClear()
    mocks.rates = { kind: 'error', diagnostic: { code: 'RESOURCE_RATES_LOAD_FAILED', message: '费率暂时无法读取', retryable: true } }
    mocks.gpuRateSelection.mockReturnValue({ rate: null, ambiguous: false })
    const errorWrapper = await mountView()
    const errorGpuSelect = errorWrapper.findAll('label').find((label) => label.text().includes('GPU 目录项'))!.get('select')
    await errorGpuSelect.setValue('gpu-container')
    const errorSubmit = errorWrapper.get('form.request-form button[type="submit"]')
    expect(errorWrapper.text()).toContain('费率暂时无法读取')
    expect(errorWrapper.text()).not.toContain('费用：免费')
    expect((errorSubmit.element as HTMLButtonElement).disabled).toBe(false)
    await errorWrapper.get('form.request-form').trigger('submit')
    await flushPromises()
    expect(mocks.create).toHaveBeenCalledTimes(1)
    errorWrapper.unmount()
  })

  it('keeps a GPU intent through catalog failure or disappearance until no GPU is selected explicitly', async () => {
    mocks.releases = {
      kind: 'success',
      data: [{ id: 'release-container', version: 1, runtimeKind: 'container', label: 'Container release', source: 'control' }],
    }
    const catalogState = reactive({
      kind: 'success' as 'success' | 'error',
      data: [{ id: 'gpu-container', class: 'nvidia-cuda', mode: 'exclusive', capacityUnits: 1, revision: 1, active: true }],
      diagnostic: undefined as unknown,
    })
    mocks.catalog = catalogState
    const wrapper = await mountView()
    const gpuSelect = wrapper.findAll('label').find((label) => label.text().includes('GPU 目录项'))!.get('select')
    await gpuSelect.setValue('gpu-container')
    const submit = wrapper.get('form.request-form button[type="submit"]')
    expect((submit.element as HTMLButtonElement).disabled).toBe(false)

    catalogState.kind = 'error'
    catalogState.data = []
    catalogState.diagnostic = { code: 'GPU_CATALOG_LOAD_FAILED', message: 'GPU 目录暂时无法读取', retryable: true }
    await flushPromises()
    expect(wrapper.text()).toContain('GPU 目录暂时无法读取')
    expect(wrapper.text()).toContain('已保留先前选择')
    expect(gpuSelect.findAll('option').map((option) => option.text())).toContain('先前选择的 GPU（当前不可用）')
    expect((submit.element as HTMLButtonElement).disabled).toBe(true)
    expect(wrapper.get('[data-testid="request-estimate-gpu-blocked"]').text()).toContain('所选 GPU 当前不可用')
    expect((wrapper.findAll('button').find((button) => button.text() === '估算费用')!.element as HTMLButtonElement).disabled).toBe(true)

    catalogState.kind = 'success'
    catalogState.data = []
    await flushPromises()
    expect(wrapper.text()).toContain('先前选择的 GPU 目录项当前不可用')
    expect((submit.element as HTMLButtonElement).disabled).toBe(true)

    await gpuSelect.setValue('')
    expect((submit.element as HTMLButtonElement).disabled).toBe(false)
    await wrapper.get('form.request-form').trigger('submit')
    await flushPromises()
    expect(mocks.create).toHaveBeenCalledTimes(1)
    expect(mocks.create.mock.calls[0][0].resources).not.toHaveProperty('gpu')
    wrapper.unmount()
  })

  it('keeps a selected release and an uncertain request identity through a release refresh failure', async () => {
    const release = { id: 'release-container', version: 1, runtimeKind: 'container', label: 'Container release', source: 'control' } as const
    const releaseState = reactive({
      kind: 'success' as 'success' | 'error',
      data: [release],
      diagnostic: undefined as unknown,
    })
    mocks.releases = releaseState
    mocks.create.mockResolvedValue(false)
    const wrapper = await mountView()
    const releaseSelect = wrapper.findAll('label').find((label) => label.text().includes('已发布版本'))!.get('select')
    const form = wrapper.get('form.request-form')
    await form.trigger('submit')
    await flushPromises()
    expect(mocks.create).toHaveBeenCalledTimes(1)
    const firstBody = mocks.create.mock.calls[0][0]
    const firstOptions = mocks.create.mock.calls[0][1]

    releaseState.kind = 'error'
    releaseState.data = []
    releaseState.diagnostic = { code: 'PROJECT_RELEASES_LOAD_FAILED', message: '版本暂时无法读取', retryable: true }
    await flushPromises()
    expect(releaseSelect.findAll('option').map((option) => option.text())).toContain('先前选择的版本（当前不可用）')
    expect(wrapper.text()).toContain('版本暂时无法读取，已保留先前选择')
    expect((wrapper.get('form.request-form button[type="submit"]').element as HTMLButtonElement).disabled).toBe(true)
    expect(mocks.create).toHaveBeenCalledTimes(1)

    releaseState.kind = 'success'
    releaseState.data = [release]
    await flushPromises()
    expect((releaseSelect.element as HTMLSelectElement).value).toBe('release-container:1')
    expect((wrapper.get('form.request-form button[type="submit"]').element as HTMLButtonElement).disabled).toBe(false)
    mocks.create.mockResolvedValue(true)
    await form.trigger('submit')
    await flushPromises()
    expect(mocks.create).toHaveBeenCalledTimes(2)
    expect(mocks.create.mock.calls[1][0]).toEqual(firstBody)
    expect(mocks.create.mock.calls[1][1]).toEqual(firstOptions)
    wrapper.unmount()
  })

  it('requires confirmation for cancellation and reclaim, then clears a target on project switch', async () => {
    mocks.requests = { kind: 'success', data: [pendingRequest()] }
    mocks.leases = { kind: 'success', data: [activeLease()] }
    const wrapper = await mountView()

    const cancelButton = wrapper.find('.resource-row__actions .danger-button')
    expect(cancelButton.exists()).toBe(true)
    await cancelButton.trigger('click')
    await flushPromises()
    expect(mocks.cancel).not.toHaveBeenCalled()
    const cancelDialog = document.body.querySelector('[role="alertdialog"]')
    expect(cancelDialog?.textContent).toContain('work-request-1')
    cancelDialog?.querySelector<HTMLButtonElement>('.filled-button')?.click()
    await flushPromises()
    expect(mocks.cancel).toHaveBeenCalledWith('request-1', 'researcher cancelled the pending resource request')

    const reclaimButton = wrapper.findAll('.resource-row__actions .danger-button')[1]
    expect(reclaimButton).toBeDefined()
    await reclaimButton.trigger('click')
    await flushPromises()
    expect(mocks.reclaim).not.toHaveBeenCalled()
    expect(document.body.querySelector('[role="alertdialog"]')?.textContent).toContain('lease-1')

    await wrapper.get('select').setValue('project-2')
    await flushPromises()
    expect(document.body.querySelector('[role="alertdialog"]')).toBeNull()
    expect(mocks.reclaim).not.toHaveBeenCalled()
    expect(projectsState.selectedProjectId).toBe('project-2')

    projectsState.select('project-1')
    await flushPromises()
    expect((wrapper.get('select').element as HTMLSelectElement).value).toBe('project-1')
  })

  it('blocks an unavailable URL project instead of falling back to the shared selection', async () => {
    const wrapper = await mountView('project-missing')

    expect(wrapper.text()).toContain('PROJECT_CONTEXT_INVALID')
    expect((wrapper.get('select').element as HTMLSelectElement).value).toBe('')
    expect(projectsState.selectedProjectId).toBe('project-1')
  })

  it('shows project Credits read-only to an ordinary member', async () => {
    authState.user.value = { profile: { roles: ['student'] } }
    const wrapper = await mountView()
    const panel = wrapper.get('[data-testid="project-credits-panel"]')
    expect(getProjectCredits).toHaveBeenCalledWith({ path: { projectId: 'project-1' } })
    expect(panel.get('[data-testid="credits-account-summary"]').text()).toContain('15.000000 CREDITS')
    expect(panel.text()).toContain('管理员发放')
    expect(panel.find('[data-testid="credits-admin-form"]').exists()).toBe(false)
    expect(panel.findAll('button').some((button) => button.text() === '冲正此条')).toBe(false)
    expect(panel.get('[data-testid="credits-read-only-note"]').text()).toContain('由平台管理员执行')
    wrapper.unmount()
  })

  it('lets a platform admin manage Credits from the same panel', async () => {
    authState.user.value = { profile: { roles: ['admin'] } }
    const wrapper = await mountView()
    const panel = wrapper.get('[data-testid="project-credits-panel"]')
    expect(panel.find('[data-testid="credits-admin-form"]').exists()).toBe(true)
    expect(panel.findAll('button').some((button) => button.text() === '冲正此条')).toBe(true)
    expect(panel.find('[data-testid="credits-read-only-note"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('requests a server estimate for the submitted specification and shows its total', async () => {
    mocks.releases = {
      kind: 'success',
      data: [{ id: 'release-container', version: 1, runtimeKind: 'container', label: 'Container release', source: 'control' }],
    }
    vi.mocked(estimateProjectResourceCost).mockResolvedValue({ data: {
      projectId: 'project-1', input: {}, total: { currency: 'CREDITS', amount: '16.000000' },
      lines: [{ rateId: 'rate-cpu', rateRevision: 2, unit: 'cpu_millicore_second', quantity: 57_600_000_000, quantityDivisor: 1000, unitQuantity: 3_600_000,
        unitPrice: { currency: 'CREDITS', amount: '1.000000' }, amount: { currency: 'CREDITS', amount: '16.000000' } }],
    }, error: undefined } as never)
    const wrapper = await mountView()
    await wrapper.findAll('button').find((button) => button.text() === '估算费用')!.trigger('click')
    await flushPromises()
    expect(estimateProjectResourceCost).toHaveBeenCalledWith({
      path: { projectId: 'project-1' },
      body: expect.objectContaining({ resources: { cpuMillicores: 2000, memoryBytes: 4 * 1024 ** 3, storageBytes: 20 * 1024 ** 3 } }),
    })
    const estimate = wrapper.get('[data-testid="request-cost-estimate"]')
    expect(estimate.get('[data-testid="cost-estimate-total"]').text()).toBe('16.000000 CREDITS')
    expect(estimate.text()).toContain('CPU · 16 核·小时')
    wrapper.unmount()
  })

  it('locates the request a notification links to', async () => {
    mocks.requests = { kind: 'success', data: [{ ...pendingRequest(), id: 'request-0', requestKey: 'other' }, pendingRequest()] }
    const wrapper = await mountView('project-1', { requestId: 'request-1' })
    const located = wrapper.get('[data-testid="resource-request-located"]')
    expect(located.attributes('id')).toBe('resource-request-request-1')
    expect(located.attributes('aria-current')).toBe('true')
    expect(wrapper.findAll('[aria-current="true"]')).toHaveLength(1)
    expect(wrapper.find('[data-testid="resource-request-not-found"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('only notes a request it cannot find, without showing anything about it', async () => {
    mocks.requests = { kind: 'success', data: [pendingRequest()] }
    const wrapper = await mountView('project-1', { requestId: 'request-hidden' })
    expect(wrapper.get('[data-testid="resource-request-not-found"]').text()).toContain('页面不会显示该申请的内容')
    expect(wrapper.find('[data-testid="resource-request-located"]').exists()).toBe(false)
    expect(wrapper.text()).not.toContain('request-hidden')
    wrapper.unmount()
  })

  it('notes a missing request when the project has no requests at all', async () => {
    mocks.requests = { kind: 'empty' }
    const wrapper = await mountView('project-1', { requestId: 'request-hidden' })
    expect(wrapper.find('[data-testid="resource-request-not-found"]').exists()).toBe(true)
    wrapper.unmount()
  })
})
