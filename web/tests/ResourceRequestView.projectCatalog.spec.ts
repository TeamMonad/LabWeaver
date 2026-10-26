import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'
import ResourceRequestView from '@/views/researcher/ResourceRequestView.vue'
import {
  getProjectCredits,
  listEnvironments,
  listEnvironmentTemplateReleases,
  listProjectCreditHolds,
  listProjectCreditLedger,
  listProjectResourceLeases,
  listProjectResourceRequests,
  listProjects,
  listResourceGpuCatalog,
  listResourceRates,
} from '@/generated/contracts'

// Only the HTTP boundary is replaced: the page runs on the real shared project
// catalog, Resource lists, release options and the Credits panel.
vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getProjectCredits: vi.fn(),
    listEnvironments: vi.fn(),
    listEnvironmentTemplateReleases: vi.fn(),
    listProjectCreditHolds: vi.fn(),
    listProjectCreditLedger: vi.fn(),
    listProjectResourceLeases: vi.fn(),
    listProjectResourceRequests: vi.fn(),
    listProjects: vi.fn(),
    listResourceGpuCatalog: vi.fn(),
    listResourceRates: vi.fn(),
  }
})

const authState = vi.hoisted(() => ({ user: { value: { expired: false, profile: { actor_id: 'researcher-1', roles: ['student'] } } } }))
vi.mock('@/composables/useAuth', () => ({ useAuth: () => authState }))

const project = (id: string, name: string) => ({
  id,
  name,
  description: null,
  ownerActorId: 'researcher-1',
  courseId: null,
  state: 'active' as const,
  revision: 1,
  createdAt: '2026-09-01T00:00:00.000Z',
  updatedAt: '2026-09-01T00:00:00.000Z',
})

const workRelease = {
  id: 'release-work',
  version: 1,
  name: 'Python Work',
  purpose: 'work',
  runtimeKind: 'container',
  withdrawal: null,
  publishedAt: '2026-09-02T00:00:00.000Z',
}

const emptyPage = { data: { items: [], nextCursor: null }, error: undefined }

/**
 * A real list answer arrives on a later task, never within the render that asked for it.
 * `Promise.withResolvers` is outside the project's ES2020 lib, so the executor form is used.
 */
function later<T>(value: T): Promise<T> {
  return new Promise((resolve) => { setTimeout(() => resolve(value), 0) })
}

async function ticks(count: number) {
  for (let index = 0; index < count; index += 1) await new Promise((resolve) => { setTimeout(resolve, 0) })
}

describe('ResourceRequestView project catalog', () => {
  it('settles on the routed project instead of reloading the catalog whenever the Credits panel mounts', async () => {
    vi.mocked(listProjects).mockImplementation(() => later({
      data: { items: [project('project-1', '课程项目'), project('project-2', '科研项目')], nextCursor: null },
      error: undefined,
    }) as never)
    vi.mocked(listProjectResourceRequests).mockResolvedValue(emptyPage as never)
    vi.mocked(listProjectResourceLeases).mockResolvedValue(emptyPage as never)
    vi.mocked(listEnvironments).mockResolvedValue(emptyPage as never)
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [workRelease], nextCursor: null }, error: undefined } as never)
    vi.mocked(listResourceGpuCatalog).mockResolvedValue(emptyPage as never)
    vi.mocked(listResourceRates).mockResolvedValue(emptyPage as never)
    vi.mocked(getProjectCredits).mockResolvedValue({
      data: {
        projectId: 'project-2', currency: 'CREDITS', balance: '10.000000', available: '10.000000', reserved: '0.000000',
        spent: '0.000000', pending: '0.000000', unknown: '0.000000', unknownCount: 0, platformAbsorbed: '0.000000', revision: 1,
      },
      error: undefined,
    } as never)
    vi.mocked(listProjectCreditLedger).mockResolvedValue(emptyPage as never)
    vi.mocked(listProjectCreditHolds).mockResolvedValue(emptyPage as never)

    const router = createRouter({
      history: createMemoryHistory(),
      routes: [{ path: '/researcher/resources', component: ResourceRequestView }],
    })
    await router.push({ path: '/researcher/resources', query: { projectId: 'project-2' } })
    await router.isReady()
    const wrapper = mount(ResourceRequestView, { global: { plugins: [router] } })
    try {
      // Long enough for many catalog round trips if mounting the panel invalidated the catalog again.
      await ticks(30)

      expect(listProjects).toHaveBeenCalledTimes(1)
      expect(wrapper.find('[data-testid="project-credits-panel"]').exists()).toBe(true)
      expect(getProjectCredits).toHaveBeenCalledTimes(1)
      expect(getProjectCredits).toHaveBeenCalledWith({ path: { projectId: 'project-2' } })
      expect(listProjectResourceRequests).toHaveBeenCalledTimes(1)
      expect(listEnvironmentTemplateReleases).toHaveBeenCalledTimes(1)

      const projectSelect = wrapper.get('.project-strip select')
      expect((projectSelect.element as HTMLSelectElement).value).toBe('project-2')
      expect(projectSelect.attributes('disabled')).toBeUndefined()
      expect(router.currentRoute.value.query.projectId).toBe('project-2')

      // The only published Work release is chosen, so the request can be submitted.
      const releaseSelect = wrapper.findAll('label').find((label) => label.text().includes('已发布版本'))!.get('select')
      expect((releaseSelect.element as HTMLSelectElement).value).toBe('release-work:1')
      expect(wrapper.get('button[type="submit"]').attributes('disabled')).toBeUndefined()
    } finally {
      wrapper.unmount()
    }
  })
})
