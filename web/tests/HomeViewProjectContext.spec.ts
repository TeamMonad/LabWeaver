import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'
import HomeView from '@/views/HomeView.vue'

const persistedProjectId = vi.hoisted(() => {
  localStorage.setItem('labweaver_project_id', 'project-1')
  return 'project-1'
})

const listProjects = vi.hoisted(() => vi.fn())
const authState = vi.hoisted(() => ({
  user: { value: { expired: false, profile: { roles: ['platform_admin'] } } },
  isLoading: { value: false },
  isAuthenticated: { value: true },
  login: vi.fn(),
}))

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, listProjects }
})

vi.mock('@/composables/useAuth', () => ({
  useAuth: () => authState,
}))

async function createWrapper() {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/:pathMatch(.*)*', component: { template: '<div />' } }],
  })
  await router.push('/')
  await router.isReady()
  const wrapper = mount(HomeView, { global: { plugins: [router] } })
  return { wrapper, router }
}

afterEach(() => {
  listProjects.mockReset()
  window.localStorage.clear()
})

describe('HomeView project navigation', () => {
  it('keeps a persisted project on a task link while the project list is loading', async () => {
    let resolveProjects!: (value: unknown) => void
    listProjects.mockReturnValue(new Promise((resolve) => { resolveProjects = resolve }))

    const { wrapper, router } = await createWrapper()
    const financeCard = wrapper.findAll('.task-card').find((card) => card.get('.card-title').text() === '预算与费用')
    expect(financeCard).toBeDefined()
    expect(financeCard!.attributes('href')).toBe(`/admin/resource-finance?projectId=${persistedProjectId}`)

    const navigation = financeCard!.trigger('click')
    resolveProjects({
      data: [{ id: persistedProjectId, name: '项目一', courseId: null }],
      error: undefined,
    })
    await navigation
    await flushPromises()

    expect(router.currentRoute.value.fullPath).toBe(`/admin/resource-finance?projectId=${persistedProjectId}`)
    wrapper.unmount()
  })
})
