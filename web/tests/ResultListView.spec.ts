import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createWebHistory } from 'vue-router'
import ResultListView from '@/views/student/ResultListView.vue'
import { listOwnProjectEvaluationResults, listProjects } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    listOwnProjectEvaluationResults: vi.fn(),
    listProjects: vi.fn(),
  }
})

const projectA = {
  id: 'project-a',
  name: 'Project A',
  description: 'Selected project',
  ownerActorId: 'student-1',
  courseId: null,
  revision: 1,
  state: 'active' as const,
  createdAt: '2026-09-08T00:00:00.000Z',
  updatedAt: '2026-09-08T00:00:00.000Z',
}

const projectB = {
  ...projectA,
  id: 'project-b',
  name: 'Project B',
}

const resultB = {
  runId: 'run-b',
  projectId: 'project-b',
  courseId: null,
  releaseId: 'release-b',
  frozenSubmissionId: 'submission-b',
  revision: 1,
  state: 'succeeded' as const,
  awardedScore: 9,
  maxScore: 10,
  createdAt: '2026-09-08T11:00:00.000Z',
  updatedAt: '2026-09-08T12:00:00.000Z',
  completedAt: '2026-09-08T12:00:00.000Z',
  steps: [],
}

const mountedWrappers: Array<{ unmount: () => void }> = []

async function mountAt(query: Record<string, string>) {
  const router = createRouter({
    history: createWebHistory(),
    routes: [
      { path: '/student/results', component: ResultListView },
      { path: '/student/results/:runId', component: { template: '<div />' } },
    ],
  })
  await router.push({ path: '/student/results', query })
  await router.isReady()
  const wrapper = mount(ResultListView, { global: { plugins: [router] } })
  mountedWrappers.push(wrapper)
  return { wrapper, router }
}

describe('ResultListView', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
    vi.mocked(listProjects).mockResolvedValue({
      data: [projectA, projectB] as never,
      error: undefined as never,
    })
    vi.mocked(listOwnProjectEvaluationResults).mockResolvedValue({
      data: { items: [resultB], nextCursor: null } as never,
      error: undefined as never,
    })
  })

  afterEach(() => {
    mountedWrappers.splice(0).forEach((wrapper) => wrapper.unmount())
  })

  it('uses a direct project route context and carries it into the detail link', async () => {
    const { wrapper } = await mountAt({ projectId: 'project-b' })

    await vi.waitFor(() => expect(wrapper.text()).toContain('run-b'))
    expect(listOwnProjectEvaluationResults).toHaveBeenCalledWith({
      path: { projectId: 'project-b' },
      query: { cursor: undefined, limit: 50 },
    })
    expect(wrapper.find('.result-link').attributes('href')).toContain('/student/results/run-b?projectId=project-b')
  })
})
