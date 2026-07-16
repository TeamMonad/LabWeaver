import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createWebHistory } from 'vue-router'
import EnvironmentEntryView from '@/views/student/EnvironmentEntryView.vue'
import {
  listEnvironmentTemplateReleases,
  getEnvironment,
  listEnvironmentEndpoints,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    listEnvironmentTemplateReleases: vi.fn(),
    createEnvironment: vi.fn(),
    getEnvironment: vi.fn(),
    listEnvironmentEndpoints: vi.fn(),
  }
})

const mockRelease = {
  id: 'release-1',
  courseId: 'course-1',
  candidateId: 'candidate-1',
  candidateRevision: 2,
  environmentSpecSha256: 'a'.repeat(64),
  runtimeKind: 'container' as const,
  releaseVersion: 1,
  publishedAt: '2026-07-11T10:00:00.000Z',
  publishedBy: 'teacher-1',
}

async function mountAt(query: Record<string, string> = {}) {
  const router = createRouter({
    history: createWebHistory(),
    routes: [{ path: '/student/environments', name: 'student-environments', component: EnvironmentEntryView }],
  })
  await router.push({ path: '/student/environments', query })
  await router.isReady()
  const wrapper = mount(EnvironmentEntryView, {
    global: { plugins: [router] },
  })
  return { wrapper, router }
}

describe('EnvironmentEntryView', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
  })

  afterEach(() => {
    vi.unstubAllEnvs()
  })

  it('shows blocked diagnostic when no course context is available', async () => {
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [] },
      error: undefined as never,
    })
    const { wrapper } = await mountAt()
    await vi.waitFor(() => expect(wrapper.text()).toContain('课程上下文未绑定'))
  })

  it('shows env fallback banner and lists environment template releases', async () => {
    vi.stubEnv('VITE_DEFAULT_COURSE_ID', 'demo-course-1')
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [mockRelease] },
      error: undefined as never,
    })
    const { wrapper } = await mountAt()
    await vi.waitFor(() => expect(wrapper.text()).toContain('当前使用部署配置中的默认课程上下文'))
    expect(wrapper.text()).toContain('容器')
    expect(wrapper.text()).toContain('release-1')
    expect(wrapper.text()).toContain('创建环境')
  })

  it('loads environment console when environmentId query is present', async () => {
    vi.stubEnv('VITE_DEFAULT_COURSE_ID', 'demo-course-1')
    vi.mocked(listEnvironmentTemplateReleases).mockResolvedValue({
      data: { items: [] },
      error: undefined as never,
    })
    vi.mocked(getEnvironment).mockResolvedValue({
      data: {
        id: 'env-1',
        courseId: 'demo-course-1',
        class: 'experiment',
        desiredState: 'running',
        eligibilityExpiresAt: '2026-07-12T10:00:00.000Z',
        endpoints: [],
        observedState: 'ready',
        operation: {
          id: 'op-1',
          acceptedAt: '2026-07-11T10:00:00.000Z',
          acceptedRevision: 1,
          actorId: 'student-1',
          attempt: 1,
          deadlineAt: '2026-07-11T10:05:00.000Z',
          kind: 'create',
          maxAttempts: 3,
          nextAttemptAt: '2026-07-11T10:00:00.000Z',
          preserveMutableDisk: false,
          providerStep: 0,
          state: 'succeeded',
          traceId: 'trace-1',
        },
        ownerId: 'student-1',
        providerBinding: 'static',
        releaseId: 'release-1',
        releaseVersion: 1,
        revision: 1,
        runtimeKind: 'container',
      },
      error: undefined as never,
    })
    vi.mocked(listEnvironmentEndpoints).mockResolvedValue({
      data: { items: [] },
      error: undefined as never,
    })
    const { wrapper } = await mountAt({ environmentId: 'env-1' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('env-1'))
    expect(wrapper.text()).toContain('ready')
    expect(wrapper.text()).toContain('启动')
  })
})
