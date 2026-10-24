import { effectScope, ref } from 'vue'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  selectCurrentGpuRate,
  selectCurrentGpuRateSelection,
  type ResourceRateOption,
  useProjectResourceOptions,
} from '@/composables/useProjectResourceOptions'

const mocks = vi.hoisted(() => ({
  apiGet: vi.fn(),
  fetchEnvironmentTemplateReleases: vi.fn(),
  listEnvironments: vi.fn(),
}))

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, listEnvironments: mocks.listEnvironments }
})

vi.mock('@/api/client', () => ({ apiClient: { get: mocks.apiGet } }))

vi.mock('@/composables/useEnvironmentTemplateReleases', () => ({
  fetchEnvironmentTemplateReleases: mocks.fetchEnvironmentTemplateReleases,
}))

const entry = { class: 'a100', mode: 'exclusive' as const }

function rate(overrides: Partial<ResourceRateOption>): ResourceRateOption {
  return {
    id: 'rate-default',
    revision: 1,
    unit: 'gpu_unit_second',
    unitQuantity: 1,
    gpuClass: 'a100',
    gpuMode: 'exclusive',
    unitPrice: { currency: 'USD', amount: '1.000000' },
    effectiveFrom: '2026-01-01T00:00:00.000Z',
    effectiveUntil: null,
    ...overrides,
  }
}

function environment(id: string) {
  return { id } as never
}

function environmentPage(items: unknown[], nextCursor: string | null = null) {
  return { data: { items, nextCursor } as never, error: undefined as never }
}

beforeEach(() => {
  mocks.apiGet.mockReset()
  mocks.fetchEnvironmentTemplateReleases.mockReset()
  mocks.listEnvironments.mockReset()
})

describe('selectCurrentGpuRate', () => {
  it('ignores future and expired intervals', () => {
    const selected = selectCurrentGpuRate(
      [
        rate({ id: 'future', revision: 3, effectiveFrom: '2026-09-09T00:00:00.000Z' }),
        rate({ id: 'current', revision: 2, effectiveFrom: '2026-09-01T00:00:00.000Z' }),
        rate({ id: 'expired', revision: 4, effectiveUntil: '2026-08-31T00:00:00.000Z' }),
      ],
      entry,
      new Date('2026-09-08T12:00:00.000Z'),
    )

    expect(selected?.id).toBe('current')
  })

  it('fails closed when valid intervals overlap across revisions', () => {
    const selected = selectCurrentGpuRate(
      [
        rate({ id: 'old', revision: 1 }),
        rate({ id: 'new', revision: 2 }),
      ],
      entry,
      new Date('2026-09-08T12:00:00.000Z'),
    )

    expect(selected).toBeNull()
  })

  it('fails closed when the newest revision is ambiguous', () => {
    const selected = selectCurrentGpuRate(
      [
        rate({ id: 'new-a', revision: 2 }),
        rate({ id: 'new-b', revision: 2 }),
      ],
      entry,
      new Date('2026-09-08T12:00:00.000Z'),
    )

    expect(selected).toBeNull()
  })

  it('reports ambiguity separately from a missing current rate', () => {
    const selection = selectCurrentGpuRateSelection(
      [
        rate({ id: 'new-a', revision: 2 }),
        rate({ id: 'new-b', revision: 2 }),
      ],
      entry,
      new Date('2026-09-08T12:00:00.000Z'),
    )

    expect(selection).toEqual({ rate: null, ambiguous: true })
    expect(selectCurrentGpuRateSelection([], entry, new Date('2026-09-08T12:00:00.000Z'))).toEqual({ rate: null, ambiguous: false })
  })
})

describe('useProjectResourceOptions environments', () => {
  it('returns complete Work options across multiple pages', async () => {
    mocks.listEnvironments
      .mockResolvedValueOnce(environmentPage([environment('environment-1')], 'cursor-2'))
      .mockResolvedValueOnce(environmentPage([environment('environment-2')]))
    mocks.fetchEnvironmentTemplateReleases.mockResolvedValue({ items: [] })
    mocks.apiGet.mockResolvedValue({ data: [], error: undefined })

    const scope = effectScope()
    const projectId = ref<string | null>('project-1')
    const courseId = ref<string | null>(null)
    let state!: ReturnType<typeof useProjectResourceOptions>
    scope.run(() => { state = useProjectResourceOptions(projectId, courseId) })

    await vi.waitFor(() => expect(state.environments).toEqual({
      kind: 'success',
      data: [environment('environment-1'), environment('environment-2')],
    }))
    expect(mocks.listEnvironments).toHaveBeenCalledTimes(2)
    expect(mocks.listEnvironments).toHaveBeenNthCalledWith(1, {
      query: { projectId: 'project-1', class: 'work', limit: 100 },
    })
    expect(mocks.listEnvironments).toHaveBeenNthCalledWith(2, {
      query: { projectId: 'project-1', class: 'work', limit: 100, cursor: 'cursor-2' },
    })
    scope.stop()
  })

  it('exposes a page failure without returning earlier pages as success', async () => {
    const pageError = {
      diagnosticCode: 'ENVIRONMENT_LIST_UNAVAILABLE',
      detail: 'Environment service unavailable',
      retryable: true,
    }
    mocks.listEnvironments
      .mockResolvedValueOnce(environmentPage([environment('environment-1')], 'cursor-2'))
      .mockResolvedValueOnce({ data: undefined, error: pageError })
    mocks.fetchEnvironmentTemplateReleases.mockResolvedValue({ items: [] })
    mocks.apiGet.mockResolvedValue({ data: [], error: undefined })

    const scope = effectScope()
    const projectId = ref<string | null>('project-1')
    const courseId = ref<string | null>(null)
    let state!: ReturnType<typeof useProjectResourceOptions>
    scope.run(() => { state = useProjectResourceOptions(projectId, courseId) })

    await vi.waitFor(() => expect(state.environments).toEqual({
      kind: 'error',
      diagnostic: {
        code: 'ENVIRONMENT_LIST_UNAVAILABLE',
        message: 'Environment service unavailable',
        retryable: true,
      },
    }))
    expect(state.environments.kind).toBe('error')
    scope.stop()
  })

  it('offers only Work releases that are not withdrawn in a project without a course, titled by the template name', async () => {
    mocks.listEnvironments.mockResolvedValue(environmentPage([]))
    const base = { runtimeKind: 'container', publishedAt: '2026-09-12T10:30:00.000Z', name: 'Python 分析' }
    mocks.fetchEnvironmentTemplateReleases.mockResolvedValue({
      items: [
        { ...base, id: 'release-work', version: 2, purpose: 'work' },
        { ...base, id: 'release-withdrawn', version: 1, purpose: 'work', withdrawal: { reasonCode: 'TEACHER_WITHDRAWN' } },
        { ...base, id: 'release-experiment', version: 3, purpose: 'experiment' },
      ],
    })
    mocks.apiGet.mockResolvedValue({ data: [], error: undefined })

    const scope = effectScope()
    let state!: ReturnType<typeof useProjectResourceOptions>
    scope.run(() => { state = useProjectResourceOptions(ref<string | null>('project-1'), ref<string | null>(null)) })

    await vi.waitFor(() => expect(state.releases.kind).toBe('success'))
    const releases = state.releases.kind === 'success' ? state.releases.data : []
    expect(releases.map((release) => release.id)).toEqual(['release-work'])
    expect(releases[0].label).toContain('Python 分析 v2')
    scope.stop()
  })

  it('also offers the non-withdrawn experiment releases of a course project, marked as course experiments', async () => {
    mocks.listEnvironments.mockResolvedValue(environmentPage([]))
    const base = { runtimeKind: 'container', publishedAt: '2026-09-12T10:30:00.000Z', name: '缓冲区实验' }
    mocks.fetchEnvironmentTemplateReleases.mockResolvedValue({
      items: [
        { ...base, id: 'release-work', version: 2, purpose: 'work', name: 'Python 分析' },
        { ...base, id: 'release-experiment', version: 3, purpose: 'experiment' },
        { ...base, id: 'release-experiment-withdrawn', version: 1, purpose: 'experiment', withdrawal: { reasonCode: 'TEACHER_WITHDRAWN' } },
      ],
    })
    mocks.apiGet.mockResolvedValue({ data: [], error: undefined })

    const scope = effectScope()
    let state!: ReturnType<typeof useProjectResourceOptions>
    scope.run(() => { state = useProjectResourceOptions(ref<string | null>('project-1'), ref<string | null>('course-1')) })

    await vi.waitFor(() => expect(state.releases.kind).toBe('success'))
    const releases = state.releases.kind === 'success' ? state.releases.data : []
    expect(releases.map((release) => [release.id, release.purpose])).toEqual([['release-work', 'work'], ['release-experiment', 'experiment']])
    expect(releases[1].label).toContain('课程实验 · 缓冲区实验 v3')
    expect(mocks.fetchEnvironmentTemplateReleases).toHaveBeenCalledWith('project-1', 'course-1')
    scope.stop()
  })

  it('keeps compute and GPU rates from a list that also holds build and model token rates', async () => {
    mocks.listEnvironments.mockResolvedValue(environmentPage([]))
    mocks.fetchEnvironmentTemplateReleases.mockResolvedValue({ items: [] })
    const cpu = rate({ id: 'rate-cpu', unit: 'cpu_millicore_second', unitQuantity: 3_600_000, gpuClass: null, gpuMode: null })
    const gpu = rate({ id: 'rate-gpu' })
    const build = rate({ id: 'rate-build', unit: 'build_second', unitQuantity: 3600, gpuClass: null, gpuMode: null, builderBinding: 'buildkit-primary-v1' })
    const model = rate({ id: 'rate-model', unit: 'llm_output_token', unitQuantity: 1_000_000, gpuClass: null, gpuMode: null, modelBinding: 'claude-sonnet-4', maxPreauthorization: '2.000000' })
    mocks.apiGet.mockImplementation(({ url }: { url: string }) => Promise.resolve(url.endsWith('/resource/rates')
      ? { data: [cpu, build, gpu, model], error: undefined }
      : { data: [], error: undefined }))

    const scope = effectScope()
    let state!: ReturnType<typeof useProjectResourceOptions>
    scope.run(() => { state = useProjectResourceOptions(ref<string | null>('project-1'), ref<string | null>(null)) })

    await vi.waitFor(() => expect(state.rates.kind).toBe('success'))
    expect(state.rates.kind === 'success' ? state.rates.data.map((item) => item.id) : []).toEqual(['rate-cpu', 'rate-gpu'])
    expect(state.gpuRateSelection({ id: 'gpu-1', class: 'a100', mode: 'exclusive', capacityUnits: 1, revision: 1, active: true }).rate?.id).toBe('rate-gpu')
    scope.stop()
  })
})
