import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive, ref } from 'vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import WorkspaceListView from '@/views/researcher/WorkspaceListView.vue'

const mocks = vi.hoisted(() => ({
  archive: vi.fn(),
  add: vi.fn(),
  directoryLoad: vi.fn(),
  directoryClear: vi.fn(),
  remove: vi.fn(),
  requests: [] as Record<string, unknown>[],
  requestState: null as Record<string, unknown> | null,
  loadResources: vi.fn(),
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
      { actorId: 'owner-1', username: 'owner', displayName: '项目教师', role: 'teacher', state: 'active', revision: 1 },
      { actorId: 'student-1', username: 'student-1', displayName: '学生一', role: 'student', state: 'active', revision: 2 },
    ],
  },
  acting: null as string | null,
  outcome: null,
  load: vi.fn(),
  add: mocks.add,
  remove: mocks.remove,
})

const directoryState = reactive({
  users: { kind: 'idle' as const } as Record<string, unknown>,
  load: mocks.directoryLoad,
  clear: mocks.directoryClear,
})

const authState = {
  user: ref({
    expired: false,
    profile: { roles: ['teacher'] },
  } as { expired: boolean; profile: { roles: string[] } } | null),
}

vi.mock('@/composables/useProjects', () => ({
  useProjects: () => projectsState,
  useProjectMemberships: () => membersState,
  useOrganizationDirectoryUsers: () => directoryState,
}))

vi.mock('@/composables/useAuth', () => ({
  useAuth: () => authState,
}))

vi.mock('@/composables/useProjectWorkEnvironments', () => ({
  useProjectWorkEnvironments: () => reactive({
    environments: { kind: 'empty' },
    outcome: null,
    load: vi.fn(),
  }),
}))

vi.mock('@/composables/useProjectResources', () => ({
  useProjectResources: () => reactive({ requests: mocks.requestState ?? { kind: 'success', data: mocks.requests }, load: mocks.loadResources }),
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
    mocks.requests = []
    mocks.requestState = null
    mocks.loadResources.mockReset()
    mocks.archive.mockReset()
    mocks.add.mockReset()
    mocks.add.mockResolvedValue(true)
    mocks.directoryLoad.mockReset()
    mocks.directoryClear.mockReset()
    mocks.remove.mockReset()
    projectsState.selectedProjectId = projectOne.id
    projectsState.selectedProject = projectOne
    authState.user.value = { expired: false, profile: { roles: ['teacher'] } }
    membersState.memberships = {
      kind: 'success',
      data: [
        { actorId: 'owner-1', username: 'owner', displayName: '项目教师', role: 'teacher', state: 'active', revision: 1 },
        { actorId: 'student-1', username: 'student-1', displayName: '学生一', role: 'student', state: 'active', revision: 2 },
      ],
    }
    directoryState.users = { kind: 'idle' }
  })

  it('shows a blocked Work allocation without an environment and keeps recovery in its project', async () => {
    mocks.requests = [
      { id: 'blocked-1', projectId: 'project-1', state: 'active', target: { kind: 'environment' }, diagnosticCode: 'LW_RESOURCE_WORK_ALLOCATION_BLOCKED' },
      { id: 'blocked-2', projectId: 'project-2', state: 'active', target: { kind: 'environment' }, diagnosticCode: 'LW_RESOURCE_WORK_ALLOCATION_BLOCKED' },
    ]
    const { wrapper, router } = await mountView()
    expect(wrapper.findAll('[role="alert"]').filter((alert) => alert.text().includes('分配失败'))).toHaveLength(1)
    expect(wrapper.get('a[href^="/researcher/resources"]').attributes('href')).toContain('projectId=project-1')
    await router.push({ path: '/researcher/workspaces', query: { projectId: 'project-2' } })
    await flushPromises()
    expect(wrapper.findAll('[role="alert"]').filter((alert) => alert.text().includes('分配失败'))).toHaveLength(1)
    expect(wrapper.get('a[href^="/researcher/resources"]').attributes('href')).toContain('projectId=project-2')
    wrapper.unmount()
  })

  it('shows resource loading and errors instead of silently treating unreadable requests as healthy', async () => {
    mocks.requestState = { kind: 'loading', message: '正在读取资源申请' }
    const loading = await mountView()
    expect(loading.wrapper.text()).toContain('正在读取资源申请')
    loading.wrapper.unmount()
    mocks.requestState = { kind: 'error', diagnostic: { code: 'RESOURCE_UNAVAILABLE', message: '资源申请暂时无法读取', retryable: true } }
    const failed = await mountView()
    expect(failed.wrapper.text()).toContain('资源申请暂时无法读取')
    const retry = failed.wrapper.findAll('button').find((button) => button.text().includes('重试'))!
    await retry.trigger('click')
    expect(mocks.loadResources).toHaveBeenCalledTimes(1)
    failed.wrapper.unmount()
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
    expect(document.body.querySelector('dialog')?.textContent).toContain('学生一')
    document.body.querySelector<HTMLButtonElement>('dialog .filled-button')?.click()
    await flushPromises()
    expect(mocks.remove).toHaveBeenCalledWith('student-1', expect.objectContaining({ actorId: 'student-1' }))
  })

  it('lets teachers search and select an enabled organization account before adding it', async () => {
    directoryState.users = {
      kind: 'success',
      data: {
        items: [
          { username: 'student-2', displayName: '学生二', enabled: true },
          { username: 'disabled-1', displayName: '已停用账号', enabled: false },
        ],
        page: 1,
        pageSize: 25,
        hasMore: true,
      },
    }
    const { wrapper } = await mountView()
    const search = wrapper.get('.directory-picker input')
    await search.setValue('student')
    await wrapper.get('.directory-picker .outlined-button').trigger('click')
    expect(mocks.directoryLoad).toHaveBeenCalledWith('student', 1)
    expect(wrapper.findAll('.directory-result')).toHaveLength(2)
    expect(wrapper.findAll('.directory-result')[1].attributes('disabled')).toBeDefined()
    await wrapper.get('.directory-pagination .text-button:last-child').trigger('click')
    expect(mocks.directoryLoad).toHaveBeenCalledWith('student', 2)

    await wrapper.find('.directory-result').trigger('click')
    expect(wrapper.text()).toContain('已选择：学生二（student-2）')
    await search.setValue('student-2')
    expect(wrapper.find('.directory-selection').exists()).toBe(false)
    expect(mocks.directoryClear).toHaveBeenCalled()
    await wrapper.find('.directory-result').trigger('click')
    await wrapper.get('.member-form').trigger('submit')
    await flushPromises()
    expect(mocks.add).toHaveBeenCalledWith({ username: 'student-2', role: 'student' })
    expect(mocks.directoryClear).toHaveBeenCalled()
    wrapper.unmount()
  })

  it('keeps the student owner input exact and does not expose directory search', async () => {
    authState.user.value = { expired: false, profile: { roles: ['student'] } }
    const { wrapper } = await mountView()
    expect(wrapper.find('.directory-picker').exists()).toBe(false)
    expect(wrapper.get('input[placeholder="输入完整用户名"]').exists()).toBe(true)
    expect(wrapper.text()).toContain('平台不会枚举组织账号')
    wrapper.unmount()
  })

  it('keeps a membership with unsynchronized metadata readable without promoting its actor id', async () => {
    membersState.memberships = {
      kind: 'success',
      data: [{ actorId: 'legacy-1', role: 'student', state: 'active', revision: 3 }],
    }
    const { wrapper } = await mountView()
    const member = wrapper.get('.member-row')
    expect(member.get('strong').text()).toBe('账号资料待同步')
    expect(member.text()).toContain('账号资料待同步')
    expect(member.get('details code').text()).toContain('legacy-1')
    wrapper.unmount()
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
