import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import WorkspaceListView from '@/views/researcher/WorkspaceListView.vue'

const mocks = vi.hoisted(() => ({
  archive: vi.fn(),
  remove: vi.fn(),
}))

const projectOne = {
  id: 'project-1',
  name: '课程项目',
  description: '课程项目描述',
  ownerActorId: 'owner-1',
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
  acting: null as string | null,
  outcome: null,
  load: vi.fn(),
  select(id: string) {
    projectsState.selectedProjectId = id
    projectsState.selectedProject = projectsState.projects.data.find((project) => project.id === id) ?? null
  },
  create: vi.fn(),
  update: vi.fn(),
  archive: mocks.archive,
})

const membersState = reactive({
  memberships: {
    kind: 'success' as const,
    data: [
      { actorId: 'owner-1', role: 'teacher', state: 'active', revision: 1 },
      { actorId: 'student-1', role: 'student', state: 'active', revision: 2 },
    ],
  },
  acting: null as string | null,
  outcome: null,
  load: vi.fn(),
  add: vi.fn(),
  remove: mocks.remove,
})

vi.mock('@/composables/useProjects', () => ({
  useProjects: () => projectsState,
  useProjectMemberships: () => membersState,
}))

vi.mock('@/composables/useProjectWorkEnvironments', () => ({
  useProjectWorkEnvironments: () => reactive({
    environments: { kind: 'empty' },
    outcome: null,
    load: vi.fn(),
  }),
}))

async function mountView(projectId = 'project-1') {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/researcher/workspaces', component: WorkspaceListView }],
  })
  await router.push({ path: '/researcher/workspaces', query: { projectId } })
  await router.isReady()
  const wrapper = mount(WorkspaceListView, { global: { plugins: [router] } })
  await flushPromises()
  return { wrapper, router }
}

describe('WorkspaceListView', () => {
  beforeEach(() => {
    mocks.archive.mockReset()
    mocks.remove.mockReset()
    projectsState.selectedProjectId = projectOne.id
    projectsState.selectedProject = projectOne
  })

  it('requires confirmation before archiving or removing a member', async () => {
    const { wrapper } = await mountView()

    await wrapper.get('button.outlined-button.danger-button').trigger('click')
    await flushPromises()
    expect(mocks.archive).not.toHaveBeenCalled()
    expect(document.body.querySelector('dialog')?.textContent).toContain('课程项目')
    document.body.querySelector<HTMLButtonElement>('dialog .filled-button')?.click()
    await flushPromises()
    expect(mocks.archive).toHaveBeenCalledWith(projectOne)

    await wrapper.get('.member-row .text-button').trigger('click')
    await flushPromises()
    expect(mocks.remove).not.toHaveBeenCalled()
    expect(document.body.querySelector('dialog')?.textContent).toContain('student-1')
    document.body.querySelector<HTMLButtonElement>('dialog .filled-button')?.click()
    await flushPromises()
    expect(mocks.remove).toHaveBeenCalledWith('student-1', expect.objectContaining({ actorId: 'student-1' }))
  })

  it('clears a confirmation when switching projects and keeps the URL project authoritative', async () => {
    const { wrapper, router } = await mountView()

    await wrapper.get('button.outlined-button.danger-button').trigger('click')
    await flushPromises()
    expect(document.body.querySelector('dialog')).not.toBeNull()

    await wrapper.findAll('.project-item')[1].trigger('click')
    await flushPromises()
    expect(document.body.querySelector('dialog')).toBeNull()
    expect(router.currentRoute.value.query.projectId).toBe('project-2')
    expect(projectsState.selectedProjectId).toBe('project-2')
  })

  it('blocks an unavailable URL project instead of showing another project detail', async () => {
    const { wrapper } = await mountView('project-missing')

    expect(wrapper.text()).toContain('项目链接无效')
    expect(wrapper.find('button.outlined-button.danger-button').exists()).toBe(false)
    expect(mocks.archive).not.toHaveBeenCalled()
    expect(projectsState.selectedProjectId).toBe('project-1')
  })
})
