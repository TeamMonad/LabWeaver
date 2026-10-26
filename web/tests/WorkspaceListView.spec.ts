import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive, ref } from 'vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import WorkspaceListView from '@/views/researcher/WorkspaceListView.vue'
import { getProjectAvailableActions } from '@/generated/contracts'
import { makeDiagnostic } from '@/types/async'
import { GRANTABLE_PROJECT_OBJECT_ROLES } from '@/utils/stateLabels'

const mocks = vi.hoisted(() => ({
  archive: vi.fn(),
  add: vi.fn(),
  directoryLoad: vi.fn(),
  directoryClear: vi.fn(),
  remove: vi.fn(),
  update: vi.fn(),
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
const projectTwo = { ...projectOne, id: 'project-2', name: '另一个项目', courseId: null, state: 'archived' }
const projectThree = { ...projectOne, id: 'project-3', name: '第三个项目', courseId: null }

const projectsState = reactive({
  projects: { kind: 'success' as const, data: [projectOne, projectTwo] as Array<typeof projectOne | typeof projectTwo> },
  selectedProjectId: projectOne.id as string | null,
  selectedProject: projectOne as typeof projectOne | null,
  acting: null as string | null,
  outcome: null as null | { kind: 'success' | 'error'; diagnostic: ReturnType<typeof makeDiagnostic> },
  load: vi.fn(),
  select(id: string) {
    projectsState.selectedProjectId = id
    projectsState.selectedProject = projectsState.projects.data.find((project) => project.id === id) ?? null
  },
  create: vi.fn(),
  update: mocks.update,
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
  outcome: null as null | { kind: 'success' | 'error'; diagnostic: ReturnType<typeof makeDiagnostic> },
  load: vi.fn(),
  add: mocks.add,
  remove: mocks.remove,
})

const directoryState = reactive({
  users: { kind: 'idle' as const } as Record<string, unknown>,
  load: mocks.directoryLoad,
  clear: mocks.directoryClear,
})

type TestProfile = { roles: string[]; actor_id?: string; sub?: string }

const authState = {
  user: ref({
    expired: false,
    profile: { roles: ['teacher'], actor_id: 'owner-1' },
  } as { expired: boolean; profile: TestProfile } | null),
}


vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getProjectAvailableActions: vi.fn(),
  }
})

/** Answers every project with these actions, as Control would for a member with one responsibility in each; the default is the owner's set. */
function mockAvailableActions(actions: string[] = ['browse', 'archive', 'edit', 'start', 'stop', 'configure_agent', 'grant_access', 'manage_members', 'view_costs', 'request_runtime_permissions']) {
  vi.mocked(getProjectAvailableActions).mockImplementation(async ({ path }) => ({
    data: {
      projectId: path.projectId,
      actorId: 'actor-owner',
      membershipRevision: 1,
      validUntil: '2099-01-01T00:00:00.000Z',
      actions,
    },
    error: undefined,
  }) as never)
}

function refusal(code: string) {
  return { kind: 'error' as const, diagnostic: makeDiagnostic(code, '你已没有这项操作的权限。', false, { status: 403 }) }
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

async function mountView(projectId = 'project-1', objectQuery: Record<string, string> = {}) {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/researcher/workspaces', component: WorkspaceListView }],
  })
  await router.push({ path: '/researcher/workspaces', query: { projectId, ...objectQuery } })
  await router.isReady()
  const wrapper = mount(WorkspaceListView, { global: { plugins: [router] } })
  await flushPromises()
  return { wrapper, router }
}

describe('WorkspaceListView', () => {
  beforeEach(() => {
    mockAvailableActions()
    mocks.requests = []
    mocks.requestState = null
    mocks.loadResources.mockReset()
    mocks.archive.mockReset()
    mocks.add.mockReset()
    mocks.add.mockResolvedValue(true)
    mocks.directoryLoad.mockReset()
    mocks.directoryClear.mockReset()
    mocks.remove.mockReset()
    mocks.update.mockReset()
    projectsState.selectedProjectId = projectOne.id
    projectsState.selectedProject = projectOne
    authState.user.value = { expired: false, profile: { roles: ['teacher'], actor_id: 'owner-1' } }
    membersState.memberships = {
      kind: 'success',
      data: [
        { actorId: 'owner-1', username: 'owner', displayName: '项目教师', role: 'teacher', state: 'active', revision: 1 },
        { actorId: 'student-1', username: 'student-1', displayName: '学生一', role: 'student', state: 'active', revision: 2 },
      ],
    }
    membersState.outcome = null
    projectsState.outcome = null
    projectsState.projects = { kind: 'success', data: [projectOne, projectTwo] }
    directoryState.users = { kind: 'idle' }
  })

  it('starts new Work on the Work page instead of the raw resource form', async () => {
    const { wrapper } = await mountView()
    const link = wrapper.findAll('a').find((anchor) => anchor.text().includes('新建 Work'))
    expect(link?.attributes('href')).toBe('/researcher/environments?projectId=project-1')
    wrapper.unmount()
  })

  it('drops the previous project\'s object ids when the shared selection switches project', async () => {
    const { wrapper, router } = await mountView('project-1', { releaseId: 'release-1', environmentId: 'env-1' })

    // The top-bar selector switches the shared selection; this page must not write the old ids back.
    projectsState.select('project-2')
    await flushPromises()

    expect(router.currentRoute.value.query).toEqual({ projectId: 'project-2' })
    wrapper.unmount()
  })

  it('drops the previous project\'s object ids when a project is chosen from the page list', async () => {
    const { wrapper, router } = await mountView('project-1', { releaseId: 'release-1' })

    const option = wrapper.findAll('[aria-label="项目列表"] button').find((button) => button.text().includes('另一个项目'))
    await option!.trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.query).toEqual({ projectId: 'project-2' })
    wrapper.unmount()
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

  it('labels project availability separately from running environment state', async () => {
    const { wrapper } = await mountView()

    expect(wrapper.get('[aria-label="项目列表"]').text()).toContain('可用')
    expect(wrapper.get('[aria-label="项目列表"]').text()).toContain('已归档')
    expect(wrapper.get('.project-detail .state-chip').text()).toBe('可用')
    expect(wrapper.get('.project-list').text()).not.toContain('运行中')
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
    expect(document.body.querySelector('[role="alertdialog"]')?.textContent).toContain('课程项目')
    document.body.querySelector<HTMLButtonElement>('[role="alertdialog"] .filled-button')?.click()
    await flushPromises()
    expect(mocks.archive).toHaveBeenCalledWith(projectOne)

    await wrapper.get('.member-row .text-button').trigger('click')
    await flushPromises()
    expect(mocks.remove).not.toHaveBeenCalled()
    expect(document.body.querySelector('[role="alertdialog"]')?.textContent).toContain('学生一')
    document.body.querySelector<HTMLButtonElement>('[role="alertdialog"] .filled-button')?.click()
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
        truncated: false,
      },
    }
    const { wrapper } = await mountView()
    const search = wrapper.get('.directory-picker input')
    await search.setValue('student')
    await wrapper.get('.directory-picker .outlined-button').trigger('click')
    expect(mocks.directoryLoad).toHaveBeenCalledWith('student')
    expect(wrapper.findAll('.directory-result')).toHaveLength(2)
    expect(wrapper.findAll('.directory-result')[1].attributes('disabled')).toBeDefined()

    await wrapper.find('.directory-result').trigger('click')
    expect(wrapper.text()).toContain('已选择：学生二（student-2）')
    await search.setValue('student-2')
    expect(wrapper.find('.directory-selection').exists()).toBe(false)
    expect(mocks.directoryClear).toHaveBeenCalled()
    await wrapper.find('.directory-result').trigger('click')
    await wrapper.get('select[aria-label="项目职责"]').setValue('editor')
    await wrapper.get('.member-form').trigger('submit')
    await flushPromises()
    expect(mocks.add).toHaveBeenCalledWith({ username: 'student-2', role: 'student', objectRole: 'editor' })
    expect(mocks.directoryClear).toHaveBeenCalled()
    wrapper.unmount()
  })

  it('keeps the student owner input exact and does not expose directory search', async () => {
    authState.user.value = { expired: false, profile: { roles: ['student'], sub: 'owner-1' } }
    const { wrapper } = await mountView()
    expect(wrapper.find('.directory-picker').exists()).toBe(false)
    expect(wrapper.get('input[placeholder="输入完整用户名"]').exists()).toBe(true)
    expect(wrapper.text()).toContain('平台不会枚举组织账号')
    expect(wrapper.get('button[type="submit"]').text()).toContain('保存项目')
    expect(wrapper.get('button.danger-button').exists()).toBe(true)
    wrapper.unmount()
  })

  it('keeps a viewer read-only, explains why, and preserves project and member visibility', async () => {
    authState.user.value = { expired: false, profile: { roles: ['student'], actor_id: 'student-1' } }
    mockAvailableActions(['browse'])
    const { wrapper } = await mountView()

    expect(wrapper.get('input.text-input').attributes('readonly')).toBeDefined()
    expect(wrapper.get('textarea.text-input').attributes('readonly')).toBeDefined()
    expect(wrapper.find('button[type="submit"]').exists()).toBe(false)
    expect(wrapper.find('button.danger-button').exists()).toBe(false)
    expect(wrapper.find('.member-form').exists()).toBe(false)
    expect(wrapper.findAll('.member-row .text-button')).toHaveLength(0)
    expect(wrapper.findAll('a').some((anchor) => anchor.text().includes('新建 Work'))).toBe(false)
    // Each closed write area says why, instead of silently disappearing.
    expect(wrapper.find('[data-testid="edit-permission-hint"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="members-permission-hint"]').exists()).toBe(true)
    expect(wrapper.text()).toContain('项目成员')
    wrapper.unmount()
  })

  it('lets an editor edit the project but not manage members', async () => {
    mockAvailableActions(['browse', 'edit', 'start', 'stop', 'configure_agent', 'grant_access', 'view_costs'])
    const { wrapper } = await mountView()

    expect(wrapper.get('button[type="submit"]').text()).toContain('保存项目')
    expect(wrapper.find('button.danger-button').exists()).toBe(false)
    expect(wrapper.find('[data-testid="archive-permission-hint"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="edit-permission-hint"]').exists()).toBe(false)
    expect(wrapper.find('.member-form').exists()).toBe(false)
    expect(wrapper.findAll('.member-row .text-button')).toHaveLength(0)
    expect(wrapper.find('[data-testid="members-permission-hint"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('offers archiving only when Control lists the archive action, whatever the project owner field says', async () => {
    // The signed-in account matches ownerActorId, but without `archive` in the answer the page must not offer it.
    authState.user.value = { expired: false, profile: { roles: ['teacher'], actor_id: 'owner-1' } }
    mockAvailableActions(['browse', 'edit', 'start', 'stop', 'configure_agent', 'grant_access', 'manage_members', 'view_costs'])
    const manager = await mountView()
    expect(manager.wrapper.find('button[type="submit"]').exists()).toBe(true)
    expect(manager.wrapper.find('button.danger-button').exists()).toBe(false)
    expect(manager.wrapper.find('[data-testid="archive-permission-hint"]').exists()).toBe(true)
    manager.wrapper.unmount()

    // An account that is not ownerActorId gets the button as soon as Control lists `archive`.
    authState.user.value = { expired: false, profile: { roles: ['teacher'], actor_id: 'someone-else' } }
    mockAvailableActions(['browse', 'archive', 'edit'])
    const owner = await mountView()
    expect(owner.wrapper.find('button.danger-button').exists()).toBe(true)
    expect(owner.wrapper.find('[data-testid="archive-permission-hint"]').exists()).toBe(false)
    owner.wrapper.unmount()
  })

  it('keeps every write closed and shows the reason while the permission read fails', async () => {
    vi.mocked(getProjectAvailableActions).mockResolvedValue({
      data: undefined,
      error: { type: 'about:blank', title: 'Unavailable', status: 503, detail: '成员服务暂时不可用。', diagnosticCode: 'LW_AUTH_MEMBERSHIP_UNAVAILABLE', retryable: true },
    } as never)
    const { wrapper } = await mountView()

    expect(wrapper.find('button[type="submit"]').exists()).toBe(false)
    expect(wrapper.find('.member-form').exists()).toBe(false)
    expect(wrapper.get('[data-testid="edit-permission-hint"]').text()).toContain('成员服务暂时不可用。')
    wrapper.unmount()
  })

  it('shows each member responsibility and adds a member with the chosen responsibility', async () => {
    membersState.memberships = {
      kind: 'success',
      data: [
        { actorId: 'owner-1', username: 'owner', displayName: '项目教师', role: 'teacher', objectRole: 'owner', state: 'active', revision: 1 },
        { actorId: 'student-1', username: 'student-1', displayName: '学生一', role: 'student', objectRole: 'operator', state: 'active', revision: 2 },
      ],
    } as never
    authState.user.value = { expired: false, profile: { roles: ['student'], sub: 'owner-1' } }
    const { wrapper } = await mountView()

    const rows = wrapper.findAll('.member-row')
    expect(rows[0].text()).toContain('项目职责：负责人')
    expect(rows[1].text()).toContain('项目职责：操作者')
    const guide = wrapper.get('[data-testid="member-object-role-guide"]')
    // Every grantable responsibility is explained before one is chosen; owner is never offered.
    expect(guide.findAll('dt')).toHaveLength(GRANTABLE_PROJECT_OBJECT_ROLES.length)
    expect(guide.findAll('dd').every((description) => description.text().length > 0)).toBe(true)
    // The manager's own responsibility changes meanwhile; adding a member re-reads it.
    mockAvailableActions(['browse'])
    await wrapper.get('input[placeholder="输入完整用户名"]').setValue('student-3')
    await wrapper.get('select[aria-label="项目职责"]').setValue('operator')
    await wrapper.get('.member-form').trigger('submit')
    await flushPromises()
    expect(mocks.add).toHaveBeenCalledWith({ username: 'student-3', role: 'student', objectRole: 'operator' })
    await vi.waitFor(() => expect(wrapper.find('.member-form').exists()).toBe(false))
    wrapper.unmount()
  })

  it('starts each new member without a responsibility', async () => {
    authState.user.value = { expired: false, profile: { roles: ['student'], sub: 'owner-1' } }
    const { wrapper } = await mountView()
    await wrapper.get('input[placeholder="输入完整用户名"]').setValue('student-3')
    await wrapper.get('select[aria-label="项目职责"]').setValue('operator')
    await wrapper.get('.member-form').trigger('submit')
    await flushPromises()
    // The next member starts without a responsibility again.
    expect(wrapper.get('[role="combobox"][aria-label="项目职责"]').text()).toContain('请选择项目职责')
    wrapper.unmount()
  })

  it('preselects no responsibility and keeps adding disabled until one is chosen', async () => {
    authState.user.value = { expired: false, profile: { roles: ['student'], sub: 'owner-1' } }
    const { wrapper } = await mountView()
    await wrapper.get('input[placeholder="输入完整用户名"]').setValue('student-3')

    expect(wrapper.get('[role="combobox"][aria-label="项目职责"]').text()).toContain('请选择项目职责')
    expect(wrapper.find('.member-object-role-required').exists()).toBe(true)
    const submit = wrapper.findAll('button').find((button) => button.text() === '添加成员')!
    expect(submit.attributes('disabled')).toBeDefined()
    await wrapper.get('.member-form').trigger('submit')
    await flushPromises()
    expect(mocks.add).not.toHaveBeenCalled()

    await wrapper.get('select[aria-label="项目职责"]').setValue('viewer')
    expect(submit.attributes('disabled')).toBeUndefined()
    wrapper.unmount()
  })

  it.each(GRANTABLE_PROJECT_OBJECT_ROLES.map((role) => [role]))('submits the %s responsibility with its contract value', async (role) => {
    authState.user.value = { expired: false, profile: { roles: ['student'], sub: 'owner-1' } }
    const { wrapper } = await mountView()
    await wrapper.get('input[placeholder="输入完整用户名"]').setValue('student-4')
    await wrapper.get('select[aria-label="项目职责"]').setValue(role)
    await wrapper.get('.member-form').trigger('submit')
    await flushPromises()
    expect(mocks.add).toHaveBeenCalledWith({ username: 'student-4', role: 'student', objectRole: role })
    wrapper.unmount()
  })

  it('never offers the owner responsibility', async () => {
    const { wrapper } = await mountView()
    const values = wrapper.get('select[aria-label="项目职责"]').findAll('option').map((option) => option.attributes('value'))
    expect(values).toEqual(['', 'viewer', 'operator', 'editor', 'manager'])
    wrapper.unmount()
  })

  it.each([
    'LW_PROJECT_GOVERNANCE_DENIED',
    'LW_PROJECT_ARCHIVED',
    'LW_PROJECT_MEMBERSHIP_CONFLICT',
    'LW_PROJECT_MEMBERSHIP_NOT_FOUND',
    'LW_PROJECT_COURSE_MEMBERSHIP_REQUIRED',
    'LW_PROJECT_OWNER_MEMBERSHIP_PROTECTED',
    'LW_ACCESS_DIRECTORY_USERNAME_INVALID',
    'LW_ACCESS_DIRECTORY_USER_NOT_FOUND',
    'LW_ACCESS_DIRECTORY_USER_DISABLED',
  ])('shows a Chinese reason before the code when the server refuses the member with %s', async (code) => {
    const serverDetail = 'The request was rejected by a fail-closed control-plane boundary.'
    membersState.outcome = { kind: 'error', diagnostic: makeDiagnostic(code, serverDetail, false, { status: 403, requestId: 'req-member-1' }) }
    const { wrapper } = await mountView()
    const banner = wrapper.get('.members-section .diagnostic-banner')
    const reason = banner.get('.diagnostic-message').text()
    expect(reason).not.toContain(serverDetail)
    expect(/[\u4e00-\u9fff]/.test(reason)).toBe(true)
    expect(banner.get('.diagnostic-code').text()).toBe(code)
    // The reason is read before the collapsed code details.
    expect(banner.text().indexOf(reason)).toBeLessThan(banner.text().indexOf(code))
    wrapper.unmount()
  })

  it('drops a chosen responsibility when the project changes, so it must be chosen again', async () => {
    projectsState.projects = { kind: 'success', data: [projectOne, projectThree, projectTwo] }
    authState.user.value = { expired: false, profile: { roles: ['student'], sub: 'owner-1' } }
    const { wrapper, router } = await mountView()
    await wrapper.get('select[aria-label="项目职责"]').setValue('manager')

    await router.push({ path: '/researcher/workspaces', query: { projectId: 'project-3' } })
    await flushPromises()
    expect(projectsState.selectedProjectId).toBe('project-3')
    await wrapper.get('input[placeholder="输入完整用户名"]').setValue('student-5')
    const submit = wrapper.findAll('button').find((button) => button.text() === '添加成员')!
    expect(submit.attributes('disabled')).toBeDefined()
    await wrapper.get('.member-form').trigger('submit')
    await flushPromises()
    expect(mocks.add).not.toHaveBeenCalled()

    await wrapper.get('select[aria-label="项目职责"]').setValue('viewer')
    await wrapper.get('.member-form').trigger('submit')
    await flushPromises()
    expect(mocks.add).toHaveBeenCalledWith({ username: 'student-5', role: 'student', objectRole: 'viewer' })
    wrapper.unmount()
  })

  it.each([
    ['saving', 'update'],
    ['archiving', 'archive'],
  ] as const)('re-reads project permissions and keeps the diagnostic after %s is refused', async (_label, operation) => {
    const { wrapper } = await mountView()
    mocks[operation].mockImplementation(async () => {
      projectsState.outcome = refusal('LW_AUTH_SCOPE_DENIED')
      return false
    })
    mockAvailableActions(['browse'])

    if (operation === 'update') {
      await wrapper.get('form.project-form').trigger('submit')
    } else {
      await wrapper.get('button.outlined-button.danger-button').trigger('click')
      await flushPromises()
      document.body.querySelector<HTMLButtonElement>('[role="alertdialog"] .filled-button')?.click()
    }
    await flushPromises()

    expect(mocks[operation]).toHaveBeenCalled()
    expect(wrapper.text()).toContain('LW_AUTH_SCOPE_DENIED')
    // The refreshed answer (viewer) closes the write the stale answer had opened.
    await vi.waitFor(() => expect(wrapper.find('button.danger-button').exists()).toBe(false))
    expect(wrapper.find('[data-testid="edit-permission-hint"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('does not re-read permissions when a project write fails for another reason', async () => {
    const { wrapper } = await mountView()
    mocks.update.mockImplementation(async () => {
      projectsState.outcome = { kind: 'error', diagnostic: makeDiagnostic('LW_REVISION_CONFLICT', 'conflict', false, { status: 409 }) }
      return false
    })
    mockAvailableActions(['browse'])
    await wrapper.get('form.project-form').trigger('submit')
    await flushPromises()
    expect(wrapper.find('button.danger-button').exists()).toBe(true)
    wrapper.unmount()
  })

  it('lets a platform administrator manage a project they do not own', async () => {
    authState.user.value = { expired: false, profile: { roles: ['admin'], actor_id: 'admin-1' } }
    directoryState.users = { kind: 'idle' }
    const { wrapper } = await mountView()

    expect(wrapper.find('input.text-input').attributes('readonly')).toBeUndefined()
    expect(wrapper.get('button[type="submit"]').text()).toContain('保存项目')
    expect(wrapper.find('button.danger-button').exists()).toBe(true)
    expect(wrapper.find('.directory-picker').exists()).toBe(true)
    expect(wrapper.find('.member-form').exists()).toBe(true)
    expect(wrapper.findAll('.member-row .text-button')).toHaveLength(1)
    wrapper.unmount()
  })

  it('keeps a teacher who is not the owner under the same project restrictions', async () => {
    authState.user.value = { expired: false, profile: { roles: ['teacher'], actor_id: 'teacher-2' } }
    mockAvailableActions(['browse'])
    const { wrapper } = await mountView()

    expect(wrapper.find('button[type="submit"]').exists()).toBe(false)
    expect(wrapper.find('button.danger-button').exists()).toBe(false)
    expect(wrapper.find('.directory-picker').exists()).toBe(false)
    expect(wrapper.find('.member-form').exists()).toBe(false)
    expect(wrapper.findAll('.member-row .text-button')).toHaveLength(0)
    wrapper.unmount()
  })

  it('clears a pending destructive action when the signed-in actor changes', async () => {
    const { wrapper } = await mountView()

    await wrapper.get('button.outlined-button.danger-button').trigger('click')
    await flushPromises()
    expect(document.body.querySelector('[role="alertdialog"]')).not.toBeNull()

    authState.user.value = { expired: false, profile: { roles: ['student'], actor_id: 'student-1' } }
    await flushPromises()
    expect(document.body.querySelector('[role="alertdialog"]')).toBeNull()
    expect(mocks.archive).not.toHaveBeenCalled()
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
    expect(document.body.querySelector('[role="alertdialog"]')).not.toBeNull()

    await wrapper.findAll('.project-item')[1].trigger('click')
    await flushPromises()
    expect(document.body.querySelector('[role="alertdialog"]')).toBeNull()
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
