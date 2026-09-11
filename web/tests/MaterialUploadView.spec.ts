import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import MaterialUploadView from '@/views/teacher/MaterialUploadView.vue'
import { getActiveProjectLlmPolicy, listProjects } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getActiveProjectLlmPolicy: vi.fn(),
    listProjects: vi.fn(),
  }
})

const mockProject = {
  id: 'project-1',
  name: 'Demo project',
  description: 'Project used by the upload view test',
  ownerActorId: 'teacher-1',
  courseId: 'course-1',
  revision: 1,
  state: 'active',
  createdAt: '2026-07-11T00:00:00.000Z',
  updatedAt: '2026-07-11T00:00:00.000Z',
}

const mockPolicy = {
  id: 'policy-1',
  projectId: 'project-1',
  courseId: 'course-1',
  revision: 3,
  activatedAt: '2026-07-11T00:00:00.000Z',
  binding: {
    runtimeBinding: 'demo-binding',
    model: 'claude-3-5-sonnet',
    claudeCodeVersion: '0.1.0',
    workerImageSha256: 'a'.repeat(64),
    runtimeConfigSha256: 'b'.repeat(64),
    maxInFlightPerWorker: 4,
  },
  deniedDataClasses: ['secret', 'token', 'private_key'],
  budget: {
    maxInputTokens: 100000,
    maxOutputTokens: 20000,
    maxRequests: 50,
    maxCostMicrousd: 1000000,
    timeoutMilliseconds: 120000,
    maxTransientRetries: 3,
    maxSchemaRepairs: 2,
  },
  studentContentMode: 'manifest_allowlist_only',
}

describe('MaterialUploadView', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
  })

  it('shows a project-context diagnostic when no accessible project is returned', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [], error: undefined as never })

    const wrapper = mount(MaterialUploadView)
    await vi.waitFor(() => expect(wrapper.text()).toContain('PROJECT_CONTEXT_MISSING'))
    expect(wrapper.text()).toContain('请先在顶部项目选择器中选择一个项目。')
    expect(getActiveProjectLlmPolicy).not.toHaveBeenCalled()
  })

  it('loads the active project policy for the selected project', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject] as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({ data: mockPolicy as never, error: undefined as never })

    const wrapper = mount(MaterialUploadView)
    await vi.waitFor(() => expect(wrapper.text()).toContain('claude-3-5-sonnet'))
    expect(wrapper.text()).toContain('secret')
    expect(wrapper.text()).toContain('rev-3 / policy-1')
    expect(getActiveProjectLlmPolicy).toHaveBeenCalledWith({ path: { projectId: 'project-1' } })
  })

  it('surfaces project policy errors as a diagnostic', async () => {
    vi.mocked(listProjects).mockResolvedValue({ data: [mockProject] as never, error: undefined as never })
    vi.mocked(getActiveProjectLlmPolicy).mockResolvedValue({
      data: undefined as never,
      error: {
        response: {
          data: {
            diagnosticCode: 'LW_ACCESS_DENIED',
            detail: '无策略读取权限',
            retryable: false,
          },
        },
      } as never,
    })

    const wrapper = mount(MaterialUploadView)
    await vi.waitFor(() => expect(wrapper.text()).toContain('无策略读取权限'))
    expect(wrapper.text()).toContain('LW_ACCESS_DENIED')
  })
})
