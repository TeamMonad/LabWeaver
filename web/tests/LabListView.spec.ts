import { nextTick, reactive } from 'vue'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import LabListView from '@/views/teacher/LabListView.vue'

const mocks = vi.hoisted(() => ({
  projects: null as unknown,
  releases: null as unknown,
  route: { query: {} as Record<string, string> },
  replace: vi.fn(),
}))

vi.mock('@/composables/useProjects', () => ({
  useProjects: () => mocks.projects,
}))

vi.mock('@/composables/useEnvironmentTemplateReleases', () => ({
  useEnvironmentTemplateReleases: () => mocks.releases,
}))

vi.mock('vue-router', async (importOriginal) => {
  const actual = await importOriginal<typeof import('vue-router')>()
  return {
    ...actual,
    useRoute: () => mocks.route,
    useRouter: () => ({ replace: mocks.replace }),
  }
})

const project = {
  id: 'project-teacher',
  name: '操作系统实验',
  courseId: 'course-os',
  ownerActorId: 'teacher-1',
  revision: 1,
  state: 'active',
  createdAt: '2026-09-01T00:00:00.000Z',
  updatedAt: '2026-09-01T00:00:00.000Z',
}

const publishedRelease = {
  id: 'release-published',
  version: 2,
  projectId: project.id,
  courseId: project.courseId,
  runtimeKind: 'container' as const,
  publishedAt: '2026-09-10T10:00:00.000Z',
  publishedBy: 'teacher-1',
  withdrawal: null,
}

const withdrawnRelease = {
  ...publishedRelease,
  id: 'release-withdrawn',
  version: 1,
  withdrawal: {
    releaseId: 'release-withdrawn',
    releaseVersion: 1,
    withdrawnAt: '2026-09-11T10:00:00.000Z',
    actorId: 'teacher-1',
    reasonCode: 'superseded',
  },
}

type ProjectState =
  | { kind: 'success'; data: typeof project[] }
  | { kind: 'loading'; message: string }

interface TestProjectsState {
  projects: ProjectState
  selectedProjectId: string | null
  selectedProject: typeof project | null
  load: ReturnType<typeof vi.fn>
  select: (id: string) => void
}

function mountView(
  releaseState = { kind: 'success', data: [publishedRelease, withdrawnRelease] } as const,
  projectState: ProjectState = { kind: 'success', data: [project] },
) {
  const state = reactive({
    projects: projectState,
    selectedProjectId: projectState.kind === 'success' ? projectState.data[0]?.id ?? null : null,
    selectedProject: projectState.kind === 'success' ? projectState.data[0] ?? null : null,
    load: vi.fn(),
    select(id: string) {
      state.selectedProjectId = id
      state.selectedProject = state.projects.kind === 'success'
        ? state.projects.data.find((item) => item.id === id) ?? null
        : null
    },
  })
  const releases = reactive({
    releases: releaseState,
    load: vi.fn(),
  })
  mocks.projects = state
  mocks.releases = releases
  return mount(LabListView, {
    global: {
      stubs: {
        RouterLink: {
          props: ['to'],
          template: '<a :data-path="typeof to === \'string\' ? to : to.path" :data-project-id="typeof to === \'string\' ? undefined : to.query?.projectId"><slot /></a>',
        },
      },
    },
  })
}

describe('LabListView', () => {
  beforeEach(() => {
    mocks.replace.mockReset()
    mocks.route.query = {}
  })

  it('shows current published releases and keeps the project on the approval link', () => {
    const wrapper = mountView()

    expect(wrapper.text()).toContain('release-published')
    expect(wrapper.text()).not.toContain('release-withdrawn')
    expect(wrapper.get('a[data-path="/teacher/approvals"]').attributes('data-project-id')).toBe(project.id)
    expect(wrapper.text()).not.toContain('数据源尚未绑定')
  })

  it('points an empty release list to the next material step', () => {
    const wrapper = mountView({ kind: 'empty' })

    expect(wrapper.text()).toContain('还没有已发布环境模板')
    expect(wrapper.get('a[data-path="/teacher/materials"]').attributes('data-project-id')).toBe(project.id)
  })

  it('does not load a project that is unavailable in the URL', () => {
    mocks.route.query = { projectId: 'project-missing' }
    const wrapper = mountView()

    expect(wrapper.text()).toContain('项目不可用')
    expect(wrapper.text()).toContain('不存在或你无权访问')
    expect(wrapper.text()).not.toContain('release-published')
    expect(wrapper.get('a[data-path="/researcher/workspaces"]').attributes('data-project-id')).toBeUndefined()
  })

  it('keeps an unavailable URL blocked when the project list resolves and preserves its first selection', async () => {
    mocks.route.query = { projectId: 'project-missing' }
    const wrapper = mountView(undefined, { kind: 'loading', message: '加载项目…' })
    const state = mocks.projects as TestProjectsState

    state.projects = { kind: 'success', data: [project] }
    state.selectedProjectId = project.id
    state.selectedProject = project
    await nextTick()

    expect(state.selectedProjectId).toBe(project.id)
    expect(mocks.replace).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('项目不可用')
    expect(wrapper.text()).not.toContain('release-published')
    expect(wrapper.get('a[data-path="/researcher/workspaces"]').attributes('data-project-id')).toBeUndefined()
  })

  it('selects the first project after the project list finishes loading', async () => {
    const wrapper = mountView(undefined, { kind: 'loading', message: '加载项目…' })
    const state = mocks.projects as TestProjectsState

    state.projects = { kind: 'success', data: [project] }
    await nextTick()

    expect(state.selectedProjectId).toBe(project.id)
    expect(wrapper.get('a[data-path="/teacher/approvals"]').attributes('data-project-id')).toBe(project.id)
  })
})
