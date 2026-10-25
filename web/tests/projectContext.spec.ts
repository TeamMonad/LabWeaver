import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { enableAutoUnmount, flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { defineComponent, h } from 'vue'
import { createMemoryHistory, createRouter, type RouteRecordRaw, type Router } from 'vue-router'
import type * as Contracts from '@/generated/contracts'
// Each test re-imports the shell after vi.resetModules() so module-level
// project state starts fresh. Loading the graph once here moves the cold
// transform of the shell and its layout components into file collection,
// which has no per-test timeout; the per-test re-import only re-evaluates.
import '@/components/layout/AppShell.vue'

const listProjects = vi.hoisted(() => vi.fn())
const getProject = vi.hoisted(() => vi.fn())

// Partial: the shell also mounts SDK consumers outside the project context
// (the unread notification count); they keep the real functions over the
// stubbed fetch instead of failing on a missing export.
vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof Contracts>()
  return { ...actual, listProjects, getProject }
})

const projectA = { id: 'project-a', name: '项目 A', state: 'active', revision: 1, ownerActorId: 'u', createdAt: '', updatedAt: '' }
const projectB = { id: 'project-b', name: '项目 B', state: 'active', revision: 1, ownerActorId: 'u', createdAt: '', updatedAt: '' }

const actorA = '01900000-0000-7000-8000-00000000000a'
const VALID_UNTIL = '2099-01-01T00:00:00.000Z'

function sessionResponse(signedIn: boolean, expiresAt = VALID_UNTIL) {
  if (!signedIn) return new Response(JSON.stringify({ status: 401 }), { status: 401, headers: { 'content-type': 'application/problem+json' } })
  return new Response(JSON.stringify({
    actor: { actorId: actorA, roles: ['teacher'], expiresAt },
    authorizationRevision: 1,
    expiresAt,
    scopes: [{ kind: 'global' }],
  }), { status: 200, headers: { 'content-type': 'application/json' } })
}

let mounts = 0
const ScopedPage = defineComponent({
  name: 'ScopedPage',
  setup() {
    mounts += 1
    return () => h('div', { class: 'scoped-page' }, 'project page')
  },
})

const routes: RouteRecordRaw[] = [
  { path: '/', component: ScopedPage, meta: { title: '进行中', projectScoped: true } },
  { path: '/researcher/environments', component: ScopedPage, meta: { title: 'Work 环境', projectScoped: true, requiredRoles: ['teacher'] } },
  { path: '/researcher/workspaces', component: ScopedPage, meta: { title: '项目与工作空间', projectScoped: true, requiredRoles: ['teacher'] } },
  { path: '/admin/gpu-catalog', component: ScopedPage, meta: { title: 'GPU 目录' } },
]

enableAutoUnmount(afterEach)

const SHELL_STUBS = { TopAppBar: true, NavigationDrawer: true }

/** Mounts the shell the way App.vue does: the session is read, then the shell renders. */
async function mountShell(path: string, { signedIn = true, awaitList = true, routeTable = routes } = {}) {
  vi.resetModules()
  const fetch = vi.fn(async () => sessionResponse(signedIn))
  vi.stubGlobal('fetch', fetch)
  const { default: AppShell } = await import('@/components/layout/AppShell.vue')
  const { useAuth } = await import('@/composables/useAuth')
  const { sessionEpoch } = await import('@/api/session')
  const { trackPendingNavigation } = await import('@/utils/pendingNavigation')
  const router = createRouter({ history: createMemoryHistory(), routes: routeTable })
  // As in the app router: installed before any guard that waits.
  trackPendingNavigation(router)
  await router.push(path)
  await router.isReady()
  const auth = useAuth()
  await auth.loadUser()
  const wrapper = mount(AppShell, {
    global: {
      plugins: [router],
      stubs: SHELL_STUBS,
    },
  })
  const { useProjects } = await import('@/composables/useProjects')
  // The real shell loads the catalog through the top bar selector.
  const load = useProjects().load()
  if (awaitList) await load
  await flushPromises()
  return { wrapper, router, projects: useProjects(), auth, fetch, sessionEpoch }
}

/** The project list stays unanswered until the test answers it. */
function heldList() {
  let answer!: (value: unknown) => void
  listProjects.mockReturnValue(new Promise((resolve) => { answer = resolve }))
  return () => answer({ data: { items: [projectA, projectB], nextCursor: null }, error: undefined })
}

type GuardOutcome = undefined | false | string | Error

/**
 * Hold navigations to `path` in a guard, as the session read of a
 * role-protected route does, until the test decides how the guard ends.
 */
function holdNavigation(router: Router, path: string) {
  let entered!: () => void
  const waiting = new Promise<void>((resolve) => { entered = resolve })
  let release!: (outcome: GuardOutcome) => void
  const outcome = new Promise<GuardOutcome>((resolve) => { release = resolve })
  router.beforeEach(async (to) => {
    if (to.path !== path) return
    entered()
    const result = await outcome
    if (result instanceof Error) throw result
    return result
  })
  return { waiting, release }
}

/** Start a navigation, keeping whatever it settles with (a failure or a guard error). */
function navigate(router: Router, path: string): Promise<unknown> {
  return router.push(path).catch((error: unknown) => error)
}

const problem = (status: number, diagnosticCode: string) => ({
  data: undefined,
  error: { status, diagnosticCode, detail: 'denied', requestId: 'req-ctx', retryable: false, title: '', type: 'about:blank', instance: '/' },
})

describe('AppShell project context', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    localStorage.clear()
    mounts = 0
    listProjects.mockReset()
    getProject.mockReset()
    listProjects.mockResolvedValue({ data: { items: [projectA, projectB], nextCursor: null }, error: undefined })
  })

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('shows the real home sign-in section instead of the project gate when nobody is signed in', async () => {
    listProjects.mockResolvedValue(problem(401, 'LW_AUTH_SESSION_REJECTED'))
    const homeRoutes: RouteRecordRaw[] = [
      { path: '/', component: () => import('@/views/HomeView.vue'), meta: { title: '进行中', projectScoped: true } },
      ...routes.slice(1),
    ]
    const { wrapper, router } = await mountShell('/', { signedIn: false, routeTable: homeRoutes })

    // The home page renders its own sign-in entry; there is no project to confirm.
    expect(wrapper.get('#login-heading').text()).toBe('欢迎进入 LabWeaver')
    expect(wrapper.get('.home-view .empty-state .filled-button').text()).toContain('登录')
    expect(wrapper.find('[data-testid="project-context-list-error"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="project-context-signed-out"]').exists()).toBe(false)
    expect(wrapper.text()).not.toContain('正在确认项目…')
    expect(router.currentRoute.value.query.projectId).toBeUndefined()
    expect(getProject).not.toHaveBeenCalled()
    expect(document.title).toBe('进行中 · LabWeaver')
  })

  it('keeps a role-protected project page behind the sign-in gate when nobody is signed in', async () => {
    listProjects.mockResolvedValue(problem(401, 'LW_AUTH_SESSION_REJECTED'))
    const { wrapper } = await mountShell('/researcher/environments?projectId=project-a', { signedIn: false })

    const gate = wrapper.get('[data-testid="project-context-signed-out"]')
    expect(gate.text()).toContain('需要重新登录')
    expect(gate.get('button').text()).toContain('重新登录')
    expect(wrapper.find('.scoped-page').exists()).toBe(false)
    expect(mounts).toBe(0)
    expect(document.title).toBe('需要重新登录 · Work 环境 · LabWeaver')
  })

  it('closes an open project page when the actor can no longer be confirmed, even if the session epoch stays', async () => {
    const { wrapper, auth, fetch, sessionEpoch } = await mountShell('/researcher/environments?projectId=project-a')
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
    const epoch = sessionEpoch.value

    // A session read that fails in transit leaves the identity unknown; the epoch is kept on purpose.
    fetch.mockRejectedValueOnce(new TypeError('Failed to fetch'))
    await auth.loadUser()
    await flushPromises()

    expect(sessionEpoch.value).toBe(epoch)
    expect(auth.isAuthenticated.value).toBe(false)
    // The list read for that actor is still cached, yet the page and its controls are gone.
    expect(wrapper.find('.scoped-page').exists()).toBe(false)
    expect(wrapper.find('[data-testid="project-context-signed-out"]').exists()).toBe(true)

    // The same actor is confirmed again: the page opens again, freshly created.
    await auth.loadUser()
    await flushPromises()
    expect(wrapper.find('[data-testid="project-context-signed-out"]').exists()).toBe(false)
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
    expect(mounts).toBe(2)
  })

  it('ends the session epoch when the session expires so the expired actor\'s list is read again', async () => {
    const { wrapper, auth, fetch, sessionEpoch } = await mountShell('/researcher/environments?projectId=project-a')
    const epoch = sessionEpoch.value
    const listCalls = listProjects.mock.calls.length

    fetch.mockResolvedValueOnce(sessionResponse(true, '2000-01-01T00:00:00.000Z'))
    await auth.loadUser()
    await flushPromises()

    expect(sessionEpoch.value).toBe(epoch + 1)
    expect(listProjects.mock.calls.length).toBeGreaterThan(listCalls)
    expect(wrapper.find('.scoped-page').exists()).toBe(false)
    expect(wrapper.find('[data-testid="project-context-signed-out"]').exists()).toBe(true)

    // A valid session afterwards starts another epoch and reads the list again before the page opens.
    const relistCalls = listProjects.mock.calls.length
    await auth.loadUser()
    await flushPromises()
    expect(sessionEpoch.value).toBe(epoch + 2)
    expect(listProjects.mock.calls.length).toBeGreaterThan(relistCalls)
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
  })

  it('scopes the home page to the current project once the actor is signed in', async () => {
    localStorage.setItem('labweaver_project_id', 'project-b')
    const { wrapper, router } = await mountShell('/')

    expect(router.currentRoute.value.query.projectId).toBe('project-b')
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
    expect(document.title).toBe('进行中 · 项目 B · LabWeaver')
  })

  describe('while a navigation is still resolving its guards', () => {
    // Each case: the list settles while the navigation waits in a guard. The
    // project must not be written into the address then (the replace would
    // cancel the navigation), and must be written once the navigation ends.
    beforeEach(() => {
      localStorage.setItem('labweaver_project_id', 'project-b')
    })

    async function settleListDuringNavigation(path: string) {
      const answerList = heldList()
      const shell = await mountShell('/', { awaitList: false })
      const held = holdNavigation(shell.router, path)
      const navigation = navigate(shell.router, path)
      await held.waiting
      answerList()
      await flushPromises()
      expect(shell.router.currentRoute.value.fullPath).toBe('/')
      return { ...shell, held, navigation }
    }

    it('lets the navigation commit and leaves an unscoped page without a project', async () => {
      const { router, held, navigation } = await settleListDuringNavigation('/admin/gpu-catalog')
      held.release(undefined)
      await navigation
      await flushPromises()
      expect(router.currentRoute.value.fullPath).toBe('/admin/gpu-catalog')
    })

    it('writes the project into the scoped page the navigation commits to', async () => {
      const { router, held, navigation } = await settleListDuringNavigation('/researcher/environments')
      held.release(undefined)
      await navigation
      await flushPromises()
      expect(router.currentRoute.value.path).toBe('/researcher/environments')
      expect(router.currentRoute.value.query.projectId).toBe('project-b')
    })

    it('writes the project into the current page once the navigation is aborted', async () => {
      const { router, held, navigation } = await settleListDuringNavigation('/researcher/environments')
      held.release(false)
      await navigation
      await flushPromises()
      expect(router.currentRoute.value.path).toBe('/')
      expect(router.currentRoute.value.query.projectId).toBe('project-b')
    })

    it('writes the project into the page a redirect ends on', async () => {
      const { router, held, navigation } = await settleListDuringNavigation('/admin/gpu-catalog')
      held.release('/researcher/workspaces')
      await navigation
      await flushPromises()
      expect(router.currentRoute.value.path).toBe('/researcher/workspaces')
      expect(router.currentRoute.value.query.projectId).toBe('project-b')
    })

    it('writes the project into the current page once a guard error ends the navigation', async () => {
      const { router, held, navigation } = await settleListDuringNavigation('/researcher/environments')
      const failure = new Error('session read failed')
      held.release(failure)
      expect(await navigation).toBe(failure)
      await flushPromises()
      expect(router.currentRoute.value.path).toBe('/')
      expect(router.currentRoute.value.query.projectId).toBe('project-b')
    })

    it('waits for a navigation a duplicate push cancelled, then writes the project', async () => {
      const { router, held, navigation } = await settleListDuringNavigation('/admin/gpu-catalog')
      // A push to the current page runs no guard; it cancels the held navigation,
      // which only reports so once its guard returns.
      await navigate(router, '/')
      await flushPromises()
      expect(router.currentRoute.value.fullPath).toBe('/')
      held.release(undefined)
      await navigation
      await flushPromises()
      expect(router.currentRoute.value.path).toBe('/')
      expect(router.currentRoute.value.query.projectId).toBe('project-b')
    })

    it('keeps waiting for a newer navigation when an older one fails', async () => {
      const { router, held: older, navigation: olderNavigation } = await settleListDuringNavigation('/admin/gpu-catalog')
      const newer = holdNavigation(router, '/researcher/environments')
      const newerNavigation = navigate(router, '/researcher/environments')
      await newer.waiting

      const failure = new Error('older session read failed')
      older.release(failure)
      expect(await olderNavigation).toBe(failure)
      await flushPromises()
      // The newer navigation is still pending: writing now would cancel it.
      expect(router.currentRoute.value.fullPath).toBe('/')

      newer.release(undefined)
      await newerNavigation
      await flushPromises()
      expect(router.currentRoute.value.path).toBe('/researcher/environments')
      expect(router.currentRoute.value.query.projectId).toBe('project-b')
    })
  })

  it('writes the project back when a navigation on the same page drops it', async () => {
    const { router } = await mountShell('/researcher/environments?projectId=project-a')
    await router.push('/researcher/environments')
    await flushPromises()
    expect(router.currentRoute.value.query.projectId).toBe('project-a')
  })

  it('keeps a project chosen in the selector while the navigation to it is still resolving', async () => {
    const { router, projects } = await mountShell('/researcher/environments?projectId=project-a')
    expect(mounts).toBe(1)
    const held = holdNavigation(router, '/researcher/environments')

    // As the top-bar selector does: choose the project, then move the address to it.
    projects.select('project-b')
    const navigation = navigate(router, '/researcher/environments?projectId=project-b')
    await held.waiting
    await flushPromises()
    // The address still names project A, but the choice must not be undone in the meantime.
    expect(projects.selectedProjectId).toBe('project-b')
    expect(mounts).toBe(1)

    held.release(undefined)
    await navigation
    await flushPromises()
    expect(router.currentRoute.value.query.projectId).toBe('project-b')
    expect(projects.selectedProjectId).toBe('project-b')
    expect(mounts).toBe(2)
  })

  it('writes the selected project into the address of a scoped page', async () => {
    localStorage.setItem('labweaver_project_id', 'project-b')
    const { router, wrapper } = await mountShell('/researcher/environments')
    expect(router.currentRoute.value.query.projectId).toBe('project-b')
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
    expect(document.title).toBe('Work 环境 · 项目 B · LabWeaver')
  })

  it('lets a deep link win over the stored selection so title, selector and requests agree', async () => {
    localStorage.setItem('labweaver_project_id', 'project-b')
    const { projects } = await mountShell('/researcher/environments?projectId=project-a')
    expect(projects.selectedProjectId).toBe('project-a')
    expect(document.title).toBe('Work 环境 · 项目 A · LabWeaver')
  })

  it('does not add a project to unscoped admin pages', async () => {
    const { router } = await mountShell('/admin/gpu-catalog')
    expect(router.currentRoute.value.query.projectId).toBeUndefined()
  })

  it('fails closed on an inaccessible linked project and never renders the previous project page', async () => {
    localStorage.setItem('labweaver_project_id', 'project-a')
    getProject.mockResolvedValue(problem(403, 'LW_ACCESS_FORBIDDEN'))
    const { wrapper } = await mountShell('/researcher/environments?projectId=project-x&environmentId=env-old')

    expect(getProject).toHaveBeenCalledWith({ path: { projectId: 'project-x' } })
    expect(wrapper.find('.scoped-page').exists()).toBe(false)
    expect(mounts).toBe(0)
    const recovery = wrapper.get('[data-testid="project-context-recovery"]')
    expect(recovery.text()).toContain('项目不存在或你无权访问。')
    expect(recovery.text()).not.toContain('成员')
    expect(recovery.text()).toContain('req-ctx')
    expect(recovery.text()).toContain('切换到「项目 A」')
    expect(document.title).toContain('项目不可用')
  })

  it('explains a missing project and recovers to an accessible one without carrying object ids', async () => {
    localStorage.setItem('labweaver_project_id', 'project-a')
    getProject.mockResolvedValue(problem(404, 'LW_PROJECT_NOT_FOUND'))
    const { wrapper, router } = await mountShell('/researcher/environments?projectId=project-x&environmentId=env-old')
    // 404 and 403 read the same: the page never asserts whether the project exists.
    expect(wrapper.text()).toContain('项目不存在或你无权访问。')

    await wrapper.get('[data-testid="project-context-recovery"] .filled-button').trigger('click')
    await flushPromises()
    expect(router.currentRoute.value.query.projectId).toBe('project-a')
    expect(router.currentRoute.value.query.environmentId).toBeUndefined()
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
  })

  it('re-creates the page when the project changes so earlier results cannot linger', async () => {
    const { router } = await mountShell('/researcher/environments?projectId=project-a')
    expect(mounts).toBe(1)
    await router.replace({ path: '/researcher/environments', query: { projectId: 'project-b' } })
    await flushPromises()
    expect(mounts).toBe(2)
  })

  it('fails closed when the project list cannot be loaded and resolves the link only after a successful retry', async () => {
    localStorage.setItem('labweaver_project_id', 'project-b')
    listProjects.mockResolvedValue(problem(503, 'LW_HTTP_SERVICE_UNAVAILABLE'))
    getProject.mockResolvedValue(problem(404, 'LW_PROJECT_NOT_FOUND'))
    const { wrapper } = await mountShell('/researcher/environments?projectId=project-x')

    expect(wrapper.find('[data-testid="project-context-list-error"]').exists()).toBe(true)
    expect(wrapper.find('.scoped-page').exists()).toBe(false)
    expect(mounts).toBe(0)
    expect(getProject).not.toHaveBeenCalled()
    expect(document.title).toContain('项目不可用')

    listProjects.mockResolvedValue({ data: { items: [projectA, projectB], nextCursor: null }, error: undefined })
    await wrapper.get('[data-testid="project-context-list-error"] .diagnostic-banner > button.text-button').trigger('click')
    await flushPromises()
    await flushPromises()
    expect(wrapper.find('[data-testid="project-context-list-error"]').exists()).toBe(false)
    // project-x is still not listed, so the shell shows recovery, never project B's page.
    expect(wrapper.get('[data-testid="project-context-recovery"]').text()).toContain('项目不存在或你无权访问。')
    expect(mounts).toBe(0)
  })

  it('treats a readable project missing from a reloaded list as unavailable instead of pairing it with the old selection', async () => {
    localStorage.setItem('labweaver_project_id', 'project-b')
    getProject.mockResolvedValue({ data: { ...projectA, id: 'project-x', name: '项目 X' }, error: undefined })
    const { wrapper, projects } = await mountShell('/?projectId=project-x')
    await flushPromises()

    // The list is reloaded before asking the server why the project is missing, and the reloads settle.
    const listCalls = listProjects.mock.calls.length
    const probeCalls = getProject.mock.calls.length
    expect(probeCalls).toBeGreaterThanOrEqual(1)
    await flushPromises()
    expect(listProjects.mock.calls.length).toBe(listCalls)
    expect(getProject.mock.calls.length).toBe(probeCalls)
    expect(wrapper.find('.scoped-page').exists()).toBe(false)
    expect(mounts).toBe(0)
    expect(wrapper.get('[data-testid="project-context-recovery"]').text()).toContain('这个项目不在你的项目列表中')
    expect(projects.selectedProjectId).toBe('project-b')
    expect(document.title).toContain('项目不可用')
  })

  it('asks again after the list changes instead of reusing an earlier denial', async () => {
    getProject.mockResolvedValue(problem(403, 'LW_ACCESS_FORBIDDEN'))
    const { wrapper, projects } = await mountShell('/researcher/environments?projectId=project-x')
    expect(wrapper.find('[data-testid="project-context-recovery"]').exists()).toBe(true)

    const projectX = { ...projectA, id: 'project-x', name: '项目 X' }
    listProjects.mockResolvedValue({ data: { items: [projectA, projectB, projectX], nextCursor: null }, error: undefined })
    await projects.load()
    await flushPromises()
    expect(wrapper.find('[data-testid="project-context-recovery"]').exists()).toBe(false)
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
    expect(projects.selectedProjectId).toBe('project-x')
  })

  it('follows the cursor to a linked project beyond the first page instead of reporting it unavailable', async () => {
    const projectX = { ...projectA, id: 'project-x', name: '项目 X' }
    listProjects.mockImplementation(async (options: { query?: { cursor?: string } }) => (
      options.query?.cursor === 'page-2'
        ? { data: { items: [projectX], nextCursor: null }, error: undefined }
        : { data: { items: [projectA, projectB], nextCursor: 'page-2' }, error: undefined }
    ))
    const { wrapper, projects } = await mountShell('/researcher/environments?projectId=project-x')
    await flushPromises()

    expect(listProjects).toHaveBeenCalledWith({ query: { limit: 50, cursor: 'page-2' } })
    expect(getProject).not.toHaveBeenCalled()
    expect(wrapper.find('[data-testid="project-context-recovery"]').exists()).toBe(false)
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
    expect(projects.selectedProjectId).toBe('project-x')
    expect(document.title).toBe('Work 环境 · 项目 X · LabWeaver')
  })

  it('keeps the loaded pages when locating a linked project fails midway and continues locating on retry', async () => {
    const projectC = { ...projectA, id: 'project-c', name: '项目 C' }
    const projectX = { ...projectA, id: 'project-x', name: '项目 X' }
    let pageThreeFails = true
    listProjects.mockImplementation(async (options: { query?: { cursor?: string } }) => {
      if (options.query?.cursor === 'page-2') return { data: { items: [projectC], nextCursor: 'page-3' }, error: undefined }
      if (options.query?.cursor === 'page-3') {
        return pageThreeFails
          ? { ...problem(503, 'CONTROL_UNAVAILABLE'), error: { ...problem(503, 'CONTROL_UNAVAILABLE').error, retryable: true } }
          : { data: { items: [projectX], nextCursor: null }, error: undefined }
      }
      return { data: { items: [projectA, projectB], nextCursor: 'page-2' }, error: undefined }
    })
    const { wrapper, router, projects } = await mountShell('/researcher/environments?projectId=project-x')
    // Imported after mountShell's vi.resetModules() so it shares this test's fresh project state.
    const { default: GcpProjectSelector } = await import('@/components/layout/GcpProjectSelector.vue')
    // Test Utils applies stubs through one global hook that the latest mount replaces;
    // keep the shell's stubs so its later re-renders do not mount the real top bar.
    const selector = mount(GcpProjectSelector, { global: { plugins: [router], stubs: SHELL_STUBS } })
    await flushPromises()

    // The failed third page is a retryable list error; pages one and two stay loaded.
    expect(listProjects).toHaveBeenLastCalledWith({ query: { limit: 50, cursor: 'page-3' } })
    expect(getProject).not.toHaveBeenCalled()
    expect(projects.projects.kind === 'success' ? projects.projects.data.map((project) => project.id) : []).toEqual(['project-a', 'project-b', 'project-c'])
    expect(wrapper.find('[data-testid="project-context-list-error"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="project-context-recovery"]').exists()).toBe(false)
    expect(wrapper.find('.scoped-page').exists()).toBe(false)
    expect(selector.find('.trigger-primary').text()).not.toBe('项目不可用')
    expect(router.currentRoute.value.query.projectId).toBe('project-x')

    pageThreeFails = false
    const callsBeforeRetry = listProjects.mock.calls.length
    await wrapper.get('[data-testid="project-context-list-error"] .diagnostic-retry').trigger('click')
    await flushPromises()

    // The retry continues from the failed page instead of restarting at page one.
    expect(listProjects.mock.calls.slice(callsBeforeRetry)).toEqual([[{ query: { limit: 50, cursor: 'page-3' } }]])
    expect(projects.projects.kind === 'success' ? projects.projects.data.map((project) => project.id) : []).toEqual(['project-a', 'project-b', 'project-c', 'project-x'])
    expect(projects.hasMore).toBe(false)
    expect(getProject).not.toHaveBeenCalled()
    expect(router.currentRoute.value.query.projectId).toBe('project-x')
    expect(projects.selectedProjectId).toBe('project-x')
    expect(selector.find('.trigger-primary').text()).toBe('项目 X')
    expect(wrapper.find('[data-testid="project-context-list-error"]').exists()).toBe(false)
    expect(wrapper.find('.scoped-page').exists()).toBe(true)
    expect(document.title).toBe('Work 环境 · 项目 X · LabWeaver')
    selector.unmount()
  })
})
