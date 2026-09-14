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
      create: vi.fn(),
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
      gpuRateSelection: () => ({ rate: null, ambiguous: false }),
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
    authState.user.value = null
    projectsState.selectedProjectId = projectOne.id
    projectsState.selectedProject = projectOne
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
