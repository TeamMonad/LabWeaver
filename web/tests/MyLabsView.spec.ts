import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { createMemoryHistory, createRouter } from 'vue-router'
import { reactive } from 'vue'
import MyLabsView from '@/views/student/MyLabsView.vue'
import {
  listEnvironmentTemplateReleases,
  listEnvironments,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    listEnvironmentTemplateReleases: vi.fn(),
    listEnvironments: vi.fn(),
    createEnvironment: vi.fn(),
    startEnvironment: vi.fn(),
    stopEnvironment: vi.fn(),
    restartEnvironment: vi.fn(),
    deleteEnvironment: vi.fn(),
  }
})

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

const projectTwo = { ...projectOne, id: 'project-2', name: 'Work 项目', courseId: null }

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

function environment(id: string, overrides: Record<string, unknown> = {}) {
  return {
    id,
    projectId: 'project-1',
    courseId: 'course-1',
    class: 'experiment',
    displayLabel: id,
    desiredState: 'running',
    eligibilityExpiresAt: '2026-09-20T00:00:00.000Z',
    endpoints: [],
    observedState: 'ready',
    ownerId: 'student-1',
    providerBinding: 'static',
    releaseId: 'release-1',
    releaseVersion: 1,
    revision: 1,
    runtimeKind: 'container',
    generation: 1,
    observedGeneration: 1,
    currentOperation: null,
    ...overrides,
  }
}

async function mountAt(projectId: string) {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/student/labs', component: MyLabsView }],
  })
  await router.push({ path: '/student/labs', query: { projectId } })
  await router.isReady()
  const wrapper = mount(MyLabsView, { global: { plugins: [router] } })
  return { wrapper, router }
}

describe('MyLabsView', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    projectsState.projects = { kind: 'success', data: [projectOne, projectTwo] }
    projectsState.selectedProjectId = projectOne.id
    projectsState.selectedProject = projectOne
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({ data: { items: [] } as never, error: undefined as never })
    vi.mocked(listEnvironments).mockResolvedValue({ data: { items: [] } as never, error: undefined as never })
    window.localStorage.clear()
  })

  it('uses the URL project context and keeps deleted console actions disabled', async () => {
    vi.mocked(listEnvironments).mockResolvedValue({
      data: {
        items: [
          environment('deleted-work', {
            class: 'work',
            displayLabel: 'Deleted Work',
            desiredState: 'deleted',
            observedState: 'deleted',
            courseId: null,
          }),
          environment('reclaiming-course', {
            displayLabel: 'Reclaiming course environment',
            desiredState: 'deleted',
            observedState: 'provisioning',
            currentOperation: { state: 'accepted' },
          }),
          environment('ready-course', { displayLabel: 'Ready course environment' }),
        ],
      } as never,
      error: undefined as never,
    })

    const { wrapper, router } = await mountAt('project-1')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Deleted Work'))

    expect(wrapper.text()).toContain('Work 项目环境')
    expect(wrapper.text()).toContain('课程实验环境')
    expect(wrapper.text()).toContain('已删除；请创建新的项目环境')
    expect(wrapper.text()).toContain('删除已请求/正在回收；请等待清理完成')

    const deletedConsole = wrapper.find('button[title*="已删除"]')
    expect(deletedConsole.exists()).toBe(true)
    expect((deletedConsole.element as HTMLButtonElement).disabled).toBe(true)
    expect(wrapper.findAll('button').some((button) => button.text().includes('控制台') && !(button.element as HTMLButtonElement).disabled)).toBe(true)
    expect(router.currentRoute.value.query.projectId).toBe('project-1')
    expect(vi.mocked(listEnvironments)).toHaveBeenCalledWith({ query: { projectId: 'project-1', courseId: 'course-1' } })
  })

  it('does not fall back to another project while the requested project is loading', async () => {
    vi.mocked(listEnvironments).mockResolvedValue({
      data: { items: [environment('work-env', { projectId: 'project-2', courseId: null, class: 'work', displayLabel: 'Work environment' })] } as never,
      error: undefined as never,
    })
    projectsState.projects = { kind: 'loading', data: [] }
    projectsState.selectedProjectId = null
    projectsState.selectedProject = null
    const { wrapper } = await mountAt('project-2')

    expect(wrapper.text()).toContain('项目上下文未绑定')
    expect(vi.mocked(listEnvironments)).not.toHaveBeenCalled()

    projectsState.projects = { kind: 'success', data: [projectOne, projectTwo] }
    projectsState.selectedProjectId = 'project-2'
    projectsState.selectedProject = projectTwo
    await vi.waitFor(() => expect(vi.mocked(listEnvironments)).toHaveBeenCalledWith({ query: { projectId: 'project-2' } }))
    expect(wrapper.text()).toContain('Work 项目环境')
  })
})
