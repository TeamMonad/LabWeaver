import { afterEach, describe, it, expect, vi, beforeEach } from 'vitest'
import { enableAutoUnmount, flushPromises, mount } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'
import HomeView from '@/views/HomeView.vue'
import { listEnvironments, listProjectAgentRuns, listProjectResourceRequests } from '@/generated/contracts'

const authState = vi.hoisted(() => ({
  user: { value: null as { expired: boolean; profile: Record<string, unknown> } | null },
  isLoading: { value: false },
  isAuthenticated: { value: false },
  login: vi.fn(),
}))

const project = { id: 'project-1', name: '量子材料实验室', state: 'active', revision: 1 }
const projectsState = vi.hoisted(() => ({
  projects: { kind: 'success', data: [] as unknown[] } as { kind: string; data?: unknown[] },
  selectedProjectId: 'project-1' as string | null,
  selectedProject: null as unknown,
  load: vi.fn(),
}))

vi.mock('@/composables/useAuth', () => ({ useAuth: () => authState }))
vi.mock('@/composables/useProjects', () => ({ useProjects: () => projectsState }))
vi.mock('@/config', () => ({ OIDC_ENABLED: true, API_BASE_URL: '/api/v1', APP_TITLE: 'LabWeaver' }))
vi.mock('@/generated/contracts', () => ({
  listEnvironments: vi.fn(),
  listProjectAgentRuns: vi.fn(),
  listProjectResourceRequests: vi.fn(),
}))

const ok = <T,>(data: T) => ({ data, error: undefined }) as never
const problem = (status: number, diagnosticCode: string, detail: string) => ({
  data: undefined,
  error: { status, diagnosticCode, detail, requestId: `req-${status}`, retryable: status >= 500, title: '', type: 'about:blank', instance: '/' },
}) as never

function environment(id: string, observedState: string, extra: Record<string, unknown> = {}) {
  return {
    id, projectId: 'project-1', class: 'work', runtimeKind: 'container', displayLabel: `环境 ${id}`,
    desiredState: 'running', observedState, currentOperation: null, ...extra,
  }
}

function run(id: string, state: string, purpose: Record<string, unknown>) {
  return { id, projectId: 'project-1', state, purpose, createdAt: '2026-10-01T00:00:00Z', updatedAt: '2026-10-01T00:00:00Z' }
}

enableAutoUnmount(afterEach)

async function createWrapper() {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/:pathMatch(.*)*', component: { template: '<div />' } }],
  })
  await router.push('/')
  await router.isReady()
  const wrapper = mount(HomeView, { global: { plugins: [router] } })
  await flushPromises()
  return { wrapper, router }
}

describe('HomeView (current project in progress)', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    authState.user.value = { expired: false, profile: { roles: ['teacher'] } }
    authState.isLoading.value = false
    authState.isAuthenticated.value = true
    projectsState.projects = { kind: 'success', data: [project] }
    projectsState.selectedProjectId = 'project-1'
    projectsState.selectedProject = project
    vi.mocked(listEnvironments).mockResolvedValue(ok({ items: [], nextCursor: null }))
    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok({ items: [], nextCursor: null }))
    vi.mocked(listProjectResourceRequests).mockResolvedValue(ok({ items: [], nextCursor: null }))
  })

  it('is not a task catalog: it shows the current project and reads only that project', async () => {
    const { wrapper } = await createWrapper()

    expect(wrapper.get('h1').text()).toBe('量子材料实验室')
    expect(wrapper.find('[data-task-group]').exists()).toBe(false)
    expect(listEnvironments).toHaveBeenCalledWith({ query: { projectId: 'project-1', desiredState: 'running', limit: 50 } })
    expect(listProjectAgentRuns).toHaveBeenCalledWith({ path: { projectId: 'project-1' }, query: { limit: 50, state: ['requested', 'running', 'awaiting_approval', 'cancelling'] } })
    expect(listProjectResourceRequests).toHaveBeenCalledWith({ path: { projectId: 'project-1' }, query: { limit: 50, state: ['reviewing', 'allocating', 'expiring', 'active'] } })
    expect(wrapper.text()).toContain('当前项目没有运行中的环境。')
  })

  it('lists running environments with a console link and flags failed ones in Chinese', async () => {
    vi.mocked(listEnvironments).mockResolvedValue(ok({
      items: [
        environment('env-ready', 'ready'),
        environment('env-failed', 'failed'),
        environment('env-starting', 'provisioning', { currentOperation: { kind: 'start', state: 'running' } }),
      ],
    }))
    const { wrapper } = await createWrapper()
    const section = wrapper.get('[data-home-section="environments"]')

    expect(section.text()).toContain('环境 env-ready')
    expect(section.text()).toContain('正在启动环境')
    expect(section.text()).toContain('环境处理失败。打开控制台查看原因')
    const hrefs = section.findAll('a').map((link) => link.attributes('href'))
    expect(hrefs).toContain('/researcher/environments?projectId=project-1&environmentId=env-ready')
    expect(section.text()).not.toMatch(/Lease|revision|AgentRun/)
  })

  it('opens the review entry directly for a run waiting for a person and hides finished runs', async () => {
    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok({
      items: [
        run('run-review', 'awaiting_approval', { kind: 'authoring', environmentClass: 'experiment' }),
        run('run-config', 'running', { kind: 'work_configuration' }),
        run('run-done', 'succeeded', { kind: 'authoring', environmentClass: 'experiment' }),
      ],
      nextCursor: null,
    }))
    const { wrapper } = await createWrapper()
    const section = wrapper.get('[data-home-section="agent-runs"]')

    expect(section.findAll('li')).toHaveLength(2)
    const review = section.findAll('a').find((link) => link.text() === '去审核')
    expect(review?.attributes('href')).toBe('/teacher/approvals?projectId=project-1&runId=run-review')
    const config = section.findAll('a').find((link) => link.text() === '查看进度')
    expect(config?.attributes('href')).toBe('/researcher/software?projectId=project-1&runId=run-config')
  })

  it('does not offer the teacher review link to an account without the teacher role', async () => {
    authState.user.value = { expired: false, profile: { roles: ['student'] } }
    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok({
      items: [run('run-review', 'awaiting_approval', { kind: 'authoring', environmentClass: 'experiment' })],
      nextCursor: null,
    }))
    const { wrapper } = await createWrapper()
    expect(wrapper.get('[data-home-section="agent-runs"]').findAll('a')).toHaveLength(0)
  })

  it('hides a section the server denies and shows other failures with a next step and request id', async () => {
    vi.mocked(listProjectAgentRuns).mockResolvedValue(problem(403, 'LW_FORBIDDEN', 'forbidden'))
    vi.mocked(listProjectResourceRequests).mockResolvedValue(problem(503, 'LW_RESOURCE_UNAVAILABLE', '资源服务暂时不可用。'))
    const { wrapper } = await createWrapper()

    expect(wrapper.find('[data-home-section="agent-runs"]').exists()).toBe(false)
    const requests = wrapper.get('[data-home-section="resource-requests"]')
    expect(requests.text()).toContain('资源服务暂时不可用。')
    expect(requests.text()).toContain('平台服务暂时不可用，请稍后重试')
    expect(requests.get('.diagnostic-request-id').text()).toBe('req-503')
  })

  it('lists pending resource requests with a readable summary', async () => {
    vi.mocked(listProjectResourceRequests).mockResolvedValue(ok({ items: [
      { id: 'req-1', projectId: 'project-1', state: 'reviewing', requestedResources: { cpuMillicores: 2000, memoryBytes: 4 * 1024 ** 3, storageBytes: 0 }, target: { kind: 'environment' }, updatedAt: '2026-10-01T00:00:00Z' },
      { id: 'req-2', projectId: 'project-1', state: 'rejected', requestedResources: { cpuMillicores: 1000, memoryBytes: 1024 ** 3, storageBytes: 0 }, target: { kind: 'environment' }, updatedAt: '2026-10-01T00:00:00Z' },
    ] , nextCursor: null }))
    const { wrapper } = await createWrapper()
    const section = wrapper.get('[data-home-section="resource-requests"]')
    expect(section.findAll('li')).toHaveLength(1)
    expect(section.text()).toContain('2 核 CPU')
    expect(section.text()).toContain('等待管理员审批')
    // The list has no paging or server filter yet, so the home page never presents it as complete.
    expect(section.get('[data-home-note="resource-requests-incomplete"]').text()).toContain('列表可能不完整')
  })

  it('does not claim the project has no pending requests when its list is empty', async () => {
    vi.mocked(listProjectResourceRequests).mockResolvedValue(ok({ items: [], nextCursor: null }))
    const { wrapper } = await createWrapper()
    const section = wrapper.get('[data-home-section="resource-requests"]')
    expect(section.text()).not.toContain('当前项目没有待审批')
    expect(section.find('[data-home-note="resource-requests-incomplete"]').exists()).toBe(true)
  })

  it('reloads when a hidden tab becomes visible instead of polling in the background', async () => {
    await createWrapper()
    expect(listEnvironments).toHaveBeenCalledTimes(1)
    const visibility = vi.spyOn(document, 'visibilityState', 'get')
    visibility.mockReturnValue('hidden')
    document.dispatchEvent(new Event('visibilitychange'))
    await flushPromises()
    expect(listEnvironments).toHaveBeenCalledTimes(1)
    visibility.mockReturnValue('visible')
    document.dispatchEvent(new Event('visibilitychange'))
    await flushPromises()
    expect(listEnvironments).toHaveBeenCalledTimes(2)
    visibility.mockRestore()
  })

  it('offers project creation when the account has no project', async () => {
    projectsState.projects = { kind: 'empty' }
    projectsState.selectedProjectId = null
    projectsState.selectedProject = null
    const { wrapper } = await createWrapper()
    expect(wrapper.text()).toContain('还没有可访问的项目')
    expect(wrapper.find('a[href="/researcher/workspaces"]').exists()).toBe(true)
    expect(listEnvironments).not.toHaveBeenCalled()
  })

  it('asks an expired session to log in without loading project data', async () => {
    authState.user.value = { expired: true, profile: { roles: ['teacher'] } }
    authState.isAuthenticated.value = false
    const { wrapper } = await createWrapper()
    expect(wrapper.get('h1').text()).toBe('欢迎进入 LabWeaver')
    expect(listEnvironments).not.toHaveBeenCalled()
  })

  it('says when only the first page was checked instead of implying the list is complete', async () => {
    vi.mocked(listEnvironments).mockResolvedValue(ok({ items: [environment('env-ready', 'ready')], nextCursor: null }))
    const { wrapper } = await createWrapper()
    expect(wrapper.get('[data-home-section="environments"]').text()).not.toContain('只显示前 50 个运行中的环境')
    expect(wrapper.get('[data-home-section="agent-runs"]').text()).not.toContain('只检查了最近 50 个任务')
  })
})
