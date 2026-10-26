import { afterEach, describe, it, expect, vi, beforeEach } from 'vitest'
import { DOMWrapper, enableAutoUnmount, flushPromises, mount } from '@vue/test-utils'
import { createRouter, createMemoryHistory } from 'vue-router'
import HomeView from '@/views/HomeView.vue'
import { getProjectAgentTaskProgress, listEnvironments, listProjectAgentRuns, listProjectResourceRequests } from '@/generated/contracts'

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
  getProjectAgentTaskProgress: vi.fn(),
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

type ProgressOptions = { path: { projectId: string; runId: string } }

const attempt = (phase: string, attemptNumber = 1) => ({ attempt: attemptNumber, phase, nextSteps: [] })

/** A task-progress body for exactly the run that was asked for. */
function progressFor(options: ProgressOptions, current: unknown[], history: unknown[] = []) {
  return {
    projectId: options.path.projectId, runId: options.path.runId, nextCursor: null,
    revision: 3, state: 'running', current, history,
  }
}

/** Visible text of each track chip: track name, phase, attempt. */
function chipTexts(scope: DOMWrapper<Element>) {
  return scope.findAll('.track-chip').map((chip) => chip.findAll('span')
    .filter((part) => part.element.parentElement === chip.element)
    .map((part) => part.text())
    .join(' '))
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
    vi.mocked(getProjectAgentTaskProgress).mockImplementation((async (options: ProgressOptions) => ok(progressFor(options, []))) as never)
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
      nextCursor: null,
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

  it('shows the current phase and attempt of each track for open runs only, without history or percentages', async () => {
    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok({
      items: [
        run('run-review', 'awaiting_approval', { kind: 'authoring', environmentClass: 'experiment' }),
        run('run-template', 'requested', { kind: 'authoring', environmentClass: 'work' }),
        run('run-config', 'running', { kind: 'work_configuration' }),
        run('run-done', 'succeeded', { kind: 'authoring', environmentClass: 'experiment' }),
      ],
      nextCursor: null,
    }))
    vi.mocked(getProjectAgentTaskProgress).mockImplementation((async (options: ProgressOptions) => {
      switch (options.path.runId) {
        case 'run-review':
          return ok(progressFor(options, [
            { track: 'evaluation', progress: attempt('awaiting_review') },
            { track: 'environment', progress: attempt('succeeded') },
          ]))
        case 'run-template':
          return ok(progressFor(options, [
            { track: 'evaluation', progress: attempt('queued', 0) },
            { track: 'environment', progress: attempt('preparing') },
          ]))
        default:
          return ok(progressFor(
            options,
            [{ track: 'work_configuration', progress: attempt('executing', 2) }],
            [{ track: 'work_configuration', progress: attempt('failed') }],
          ))
      }
    }) as never)
    const { wrapper } = await createWrapper()
    const section = wrapper.get('[data-home-section="agent-runs"]')

    expect(getProjectAgentTaskProgress).toHaveBeenCalledTimes(3)
    expect(getProjectAgentTaskProgress).toHaveBeenCalledWith({ path: { projectId: 'project-1', runId: 'run-review' }, query: { limit: 100 } })
    expect(vi.mocked(getProjectAgentTaskProgress).mock.calls.map(([options]) => options.path.runId)).not.toContain('run-done')
    expect(chipTexts(section.get('[data-run-id="run-review"]'))).toEqual(['环境 已完成 第 1 次尝试', '评测 等待人工审核 第 1 次尝试'])
    // Evaluation reports attempt 0 before its step is claimed; no attempt count is invented.
    expect(chipTexts(section.get('[data-run-id="run-template"]'))).toEqual(['环境 准备中 第 1 次尝试', '评测 排队中'])
    expect(chipTexts(section.get('[data-run-id="run-config"]'))).toEqual(['Work 配置 执行中 第 2 次尝试'])
    expect(section.text()).not.toContain('失败')
    expect(section.text()).not.toContain('%')
    expect(section.find('[role="progressbar"]').exists()).toBe(false)
    // The tooltip explains the phase in Chinese; the colour category never leaks as a state string.
    const pills = section.get('[data-run-id="run-review"]').findAll('.track-chip .gcp-status-pill')
    expect(pills.map((pill) => pill.attributes('title'))).toEqual(['这次尝试已完成。', '这次尝试已执行完，等待人工审核后才会继续。'])
    expect(section.findAll('.track-chip .gcp-status-pill').map((pill) => pill.attributes('title')).join(' ')).not.toMatch(/[a-z_]{4,}/)
  })

  it('says in Chinese what a person has to do for each waiting track and links a resource wait to the requests', async () => {
    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok({
      items: [
        run('run-review', 'awaiting_approval', { kind: 'authoring', environmentClass: 'experiment' }),
        run('run-config', 'running', { kind: 'work_configuration' }),
      ],
      nextCursor: null,
    }))
    vi.mocked(getProjectAgentTaskProgress).mockImplementation((async (options: ProgressOptions) => ok(progressFor(
      options,
      options.path.runId === 'run-review'
        ? [{ track: 'environment', progress: attempt('awaiting_review') }, { track: 'evaluation', progress: attempt('awaiting_review') }]
        : [{ track: 'work_configuration', progress: attempt('waiting_resource_approval', 2) }],
    ))) as never)
    const { wrapper } = await createWrapper()
    const section = wrapper.get('[data-home-section="agent-runs"]')

    const review = section.get('[data-run-id="run-review"]')
    expect(review.findAll('[data-run-attention]').map((hint) => hint.text())).toEqual([
      '环境已执行完，等待人工审核后才会继续。',
      '评测已执行完，等待人工审核后才会继续。',
    ])
    expect(review.classes()).toContain('item-row--attention')
    expect(review.find('.track-resource-link').exists()).toBe(false)
    expect(review.findAll('a').find((link) => link.text() === '去审核')?.attributes('href')).toBe('/teacher/approvals?projectId=project-1&runId=run-review')

    const config = section.get('[data-run-id="run-config"]')
    expect(config.get('[data-run-attention]').text()).toBe('Work 配置正在等待资源审批，批准后自动继续。')
    expect(config.classes()).toContain('item-row--attention')
    expect(config.get('.track-resource-link').attributes('href')).toBe('/researcher/resources?projectId=project-1')
  })

  it('shows a failed progress read only on that run, with a Chinese reason and request id, and retries just that run', async () => {
    vi.mocked(listEnvironments).mockResolvedValue(ok({ items: [environment('env-ready', 'ready')], nextCursor: null }))
    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok({
      items: [
        run('run-broken', 'awaiting_approval', { kind: 'authoring', environmentClass: 'experiment' }),
        run('run-config', 'running', { kind: 'work_configuration' }),
      ],
      nextCursor: null,
    }))
    vi.mocked(getProjectAgentTaskProgress).mockImplementation((async (options: ProgressOptions) => options.path.runId === 'run-broken'
      ? problem(503, 'LW_AGENT_UNAVAILABLE', 'Agent 服务暂时不可用。')
      : ok(progressFor(options, [{ track: 'work_configuration', progress: attempt('executing') }]))) as never)
    const { wrapper } = await createWrapper()
    const section = wrapper.get('[data-home-section="agent-runs"]')

    const broken = section.get('[data-run-id="run-broken"]')
    expect(broken.find('.track-chip').exists()).toBe(false)
    expect(broken.get('.diagnostic-message').text()).toBe('Agent 服务暂时不可用。')
    expect(broken.get('.diagnostic-code').text()).toBe('LW_AGENT_UNAVAILABLE')
    expect(broken.get('.diagnostic-request-id').text()).toBe('req-503')
    // The row stays usable: aggregate hint and the review link still work.
    expect(broken.text()).toContain('等待人工审核。审核通过后才会继续。')
    expect(broken.findAll('a').find((link) => link.text() === '去审核')).toBeTruthy()

    const config = section.get('[data-run-id="run-config"]')
    expect(chipTexts(config)).toEqual(['Work 配置 执行中 第 1 次尝试'])
    expect(config.find('.diagnostic-banner').exists()).toBe(false)
    expect(wrapper.get('[data-home-section="environments"]').text()).toContain('环境 env-ready')
    expect(wrapper.findAll('.diagnostic-banner')).toHaveLength(1)

    vi.mocked(getProjectAgentTaskProgress).mockClear()
    vi.mocked(getProjectAgentTaskProgress).mockImplementation((async (options: ProgressOptions) => ok(progressFor(
      options,
      [{ track: 'evaluation', progress: attempt('awaiting_review') }],
    ))) as never)
    await broken.get('.diagnostic-retry').trigger('click')
    await flushPromises()

    expect(getProjectAgentTaskProgress).toHaveBeenCalledTimes(1)
    expect(getProjectAgentTaskProgress).toHaveBeenCalledWith({ path: { projectId: 'project-1', runId: 'run-broken' }, query: { limit: 100 } })
    expect(chipTexts(section.get('[data-run-id="run-broken"]'))).toEqual(['评测 等待人工审核 第 1 次尝试'])
    expect(wrapper.find('.diagnostic-banner').exists()).toBe(false)
    expect(listProjectAgentRuns).toHaveBeenCalledTimes(1)
  })

  it('discards a progress response that belongs to another project or run', async () => {
    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok({
      items: [
        run('run-a', 'running', { kind: 'work_configuration' }),
        run('run-b', 'running', { kind: 'work_configuration' }),
      ],
      nextCursor: null,
    }))
    const tracks = [{ track: 'work_configuration', progress: attempt('awaiting_review') }]
    vi.mocked(getProjectAgentTaskProgress).mockImplementation((async (options: ProgressOptions) => ok(options.path.runId === 'run-a'
      ? { ...progressFor(options, tracks), projectId: 'project-other' }
      : { ...progressFor(options, tracks), runId: 'run-other' })) as never)
    const { wrapper } = await createWrapper()
    const section = wrapper.get('[data-home-section="agent-runs"]')

    for (const runId of ['run-a', 'run-b']) {
      const row = section.get(`[data-run-id="${runId}"]`)
      expect(row.find('.track-chip').exists()).toBe(false)
      expect(row.find('[data-run-attention]').exists()).toBe(false)
      expect(row.get('.diagnostic-code').text()).toBe('HOME_AGENT_PROGRESS_STALE_CONTEXT')
      expect(row.get('.diagnostic-message').text()).toContain('任务进度返回了其他项目或任务的数据')
    }
  })

  it('reads progress for at most four runs at a time', async () => {
    const ids = ['run-1', 'run-2', 'run-3', 'run-4', 'run-5', 'run-6']
    vi.mocked(listProjectAgentRuns).mockResolvedValue(ok({ items: ids.map((id) => run(id, 'running', { kind: 'work_configuration' })), nextCursor: null }))
    const pending: Array<() => void> = []
    let inFlight = 0
    let peak = 0
    vi.mocked(getProjectAgentTaskProgress).mockImplementation(((options: ProgressOptions) => new Promise((resolve) => {
      inFlight += 1
      peak = Math.max(peak, inFlight)
      pending.push(() => {
        inFlight -= 1
        resolve(ok(progressFor(options, [{ track: 'work_configuration', progress: attempt('executing') }])))
      })
    })) as never)
    const { wrapper } = await createWrapper()
    const section = wrapper.get('[data-home-section="agent-runs"]')

    expect(getProjectAgentTaskProgress).toHaveBeenCalledTimes(4)
    expect(section.findAll('.track-loading')).toHaveLength(6)
    while (pending.length) {
      pending.shift()?.()
      await flushPromises()
    }
    expect(getProjectAgentTaskProgress).toHaveBeenCalledTimes(6)
    expect(peak).toBe(4)
    expect(section.findAll('.track-chip')).toHaveLength(6)
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
