import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import MaterialUploadView from '@/views/teacher/MaterialUploadView.vue'
import { getActiveCourseLlmPolicy } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getActiveCourseLlmPolicy: vi.fn(),
    createProblemPackageUpload: vi.fn(),
    completeProblemPackageUpload: vi.fn(),
    createAgentRun: vi.fn(),
    getAgentRun: vi.fn(),
    cancelAgentRun: vi.fn(),
    retryAgentRunTrack: vi.fn(),
  }
})

const mockPolicy = {
  id: 'policy-1',
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

  afterEach(() => {
    vi.unstubAllEnvs()
  })

  it('shows blocked diagnostic when no course context is available', async () => {
    vi.mocked(getActiveCourseLlmPolicy).mockResolvedValue({
      data: undefined as never,
      error: undefined as never,
    })
    const wrapper = mount(MaterialUploadView)
    await vi.waitFor(() => wrapper.text().includes('课程上下文未绑定'))
    expect(wrapper.text()).toContain('课程上下文未绑定')
  })

  it('shows demo banner and loads active LLM policy from env fallback', async () => {
    vi.stubEnv('VITE_DEMO_COURSE_ID', 'demo-course-1')
    vi.mocked(getActiveCourseLlmPolicy).mockResolvedValue({
      data: mockPolicy,
      error: undefined as never,
    })
    const wrapper = mount(MaterialUploadView)
    await vi.waitFor(() => wrapper.text().includes('演示课程上下文'))
    expect(wrapper.text()).toContain('演示课程上下文')
    expect(wrapper.text()).toContain('claude-3-5-sonnet')
    expect(wrapper.text()).toContain('secret')
  })

  it('surfaces policy load errors as diagnostic', async () => {
    vi.stubEnv('VITE_DEMO_COURSE_ID', 'demo-course-1')
    vi.mocked(getActiveCourseLlmPolicy).mockResolvedValue({
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
    await vi.waitFor(() => wrapper.text().includes('无策略读取权限'))
    expect(wrapper.text()).toContain('LW_ACCESS_DENIED')
  })
})
