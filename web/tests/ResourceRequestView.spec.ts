import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import ResourceRequestView from '@/views/researcher/ResourceRequestView.vue'

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

async function mountView(projectId = 'project-1') {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/researcher/resources', component: ResourceRequestView }],
  })
  await router.push({ path: '/researcher/resources', query: { projectId } })
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
    document.body.querySelector<HTMLButtonElement>('dialog .filled-button')?.click()
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
    expect(wrapper.text()).toContain('当前项目没有可用的已发布版本')
    expect(wrapper.find('a[href^="/researcher/software"]').exists()).toBe(true)
    expect(wrapper.find('a[href^="/teacher/materials"]').exists()).toBe(false)
  })

  it('keeps the teacher-only publication link hidden for an admin without teacher role', async () => {
    authState.user.value = { profile: { roles: ['admin'] } }
    mocks.releases = { kind: 'empty' }
    const wrapper = await mountView()

    expect(wrapper.find('a[href^="/teacher/materials"]').exists()).toBe(false)
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
    const cancelDialog = document.body.querySelector('dialog')
    expect(cancelDialog?.textContent).toContain('work-request-1')
    cancelDialog?.querySelector<HTMLButtonElement>('.filled-button')?.click()
    await flushPromises()
    expect(mocks.cancel).toHaveBeenCalledWith('request-1', 'researcher cancelled the pending resource request')

    const reclaimButton = wrapper.findAll('.resource-row__actions .danger-button')[1]
    expect(reclaimButton).toBeDefined()
    await reclaimButton.trigger('click')
    await flushPromises()
    expect(mocks.reclaim).not.toHaveBeenCalled()
    expect(document.body.querySelector('dialog')?.textContent).toContain('lease-1')

    await wrapper.get('select').setValue('project-2')
    await flushPromises()
    expect(document.body.querySelector('dialog')).toBeNull()
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
})
