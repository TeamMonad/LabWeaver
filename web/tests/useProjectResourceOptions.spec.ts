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
})
