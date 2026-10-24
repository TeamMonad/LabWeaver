import { flushPromises } from '@vue/test-utils'
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { ref } from 'vue'
import { estimateProjectBuildCost, estimateProjectResourceCost } from '@/generated/contracts'
import {
  billingUnitLabel,
  formatEstimateQuantity,
  useProjectBuildCostEstimate,
  useProjectResourceCostEstimate,
} from '@/composables/useProjectResourceCostEstimate'
import { creditHoldUnknownStatus as holdStatus } from '@/composables/useProjectCredits'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    estimateProjectBuildCost: vi.fn(),
    estimateProjectResourceCost: vi.fn(),
  }
})

const estimateBody = {
  projectId: 'project-1',
  input: {
    computeFrom: '2026-10-12T12:00:00.000Z',
    computeUntil: '2026-10-12T20:00:00.000Z',
    storageFrom: '2026-10-12T12:00:00.000Z',
    storageUntil: '2026-10-12T20:00:00.000Z',
    resources: {
      cpuMillicores: 2000,
      memoryBytes: 4 * 1024 ** 3,
      storageBytes: 20 * 1024 ** 3,
    },
  },
  lines: [{
    amount: { amount: '12.000000', currency: 'CREDITS' },
    quantity: 400,
    quantityDivisor: 1000 as const,
    rateId: 'rate-cpu',
    rateRevision: 3,
    unit: 'cpu_millicore_second' as const,
    unitPrice: { amount: '0.000001', currency: 'CREDITS' },
    unitQuantity: 1,
  }],
  total: { amount: '12.000000', currency: 'CREDITS' },
}

describe('useProjectResourceCostEstimate', () => {
  beforeEach(() => {
    vi.mocked(estimateProjectResourceCost).mockReset()
    vi.mocked(estimateProjectBuildCost).mockReset()
  })

  it('formats quantity from quantity and quantityDivisor in readable units', () => {
    // 28,800,000 millicore-seconds = 2 cores × 4 h = 8 core-hours.
    expect(formatEstimateQuantity({ quantity: 28_800_000, quantityDivisor: 1, unit: 'cpu_millicore_second' })).toBe('8 核·小时')
    // 400 ms of one GPU unit with divisor 1000 is 0.4 unit-seconds, not 400 and not 0.
    expect(formatEstimateQuantity({ quantity: 400, quantityDivisor: 1000, unit: 'gpu_unit_second' })).toBe('约 0.0001 分配单位·小时')
    expect(formatEstimateQuantity({ quantity: 90_500, quantityDivisor: 1000, unit: 'build_second' })).toBe('90.5 秒')
    expect(formatEstimateQuantity({ quantity: 1_500_000, unit: 'llm_input_token' })).toBe('1,500,000 token')
    expect(billingUnitLabel('gpu_unit_second')).toBe('GPU')
    expect(billingUnitLabel('llm_cache_read_token')).toBe('模型缓存读取 token')
    expect(billingUnitLabel('future_unit')).toBe('future_unit')
  })

  it('loads server estimate and keeps server total', async () => {
    vi.mocked(estimateProjectResourceCost).mockResolvedValue({
      data: estimateBody,
      error: undefined,
    } as never)

    const projectId = ref<string | null>('project-1')
    const cost = useProjectResourceCostEstimate(projectId)
    const ok = await cost.run(estimateBody.input)
    await flushPromises()

    expect(ok).toBe(true)
    expect(estimateProjectResourceCost).toHaveBeenCalledWith({
      path: { projectId: 'project-1' },
      body: estimateBody.input,
    })
    expect(cost.estimate.value.kind).toBe('success')
    if (cost.estimate.value.kind === 'success') {
      expect(cost.estimate.value.data.total.amount).toBe('12.000000')
      expect(cost.estimate.value.data.lines[0]?.quantityDivisor).toBe(1000)
    }
  })

  it.each([
    ['LW_RESOURCE_RATE_UNCONFIGURED', '缺少适用费率'],
    ['LW_RESOURCE_GPU_CATALOG_MISSING', '当前不可用'],
    ['LW_RESOURCE_ESTIMATE_CHANGED', '原估价已失效'],
  ])('surfaces %s as an explicit Chinese error, not a zero estimate', async (code, reason) => {
    vi.mocked(estimateProjectResourceCost).mockResolvedValue({
      data: undefined,
      error: { diagnosticCode: code, detail: 'raw detail', status: 409, title: 'Conflict', type: 'about:blank', requestId: 'req-1' },
    } as never)

    const cost = useProjectResourceCostEstimate(ref<string | null>('project-1'))
    await cost.run(estimateBody.input)
    await flushPromises()

    expect(cost.estimate.value.kind).toBe('error')
    if (cost.estimate.value.kind === 'error') {
      expect(cost.estimate.value.diagnostic.code).toBe(code)
      expect(cost.estimate.value.diagnostic.message).toContain(reason)
      expect(cost.estimate.value.diagnostic.message).not.toMatch(/0\.000000|免费结算/)
      expect(cost.estimate.value.diagnostic.requestId).toBe('req-1')
    }
  })

  it('drops a late estimate after the project changes', async () => {
    // `Promise.withResolvers` is outside the project's ES2020 lib (Node 20), so the executor form captures the resolver.
    let resolve!: (value: unknown) => void
    vi.mocked(estimateProjectResourceCost).mockReturnValue(new Promise((done) => { resolve = done }) as never)
    const projectId = ref<string | null>('project-1')
    const cost = useProjectResourceCostEstimate(projectId)
    const pending = cost.run(estimateBody.input)
    projectId.value = 'project-2'
    await flushPromises()
    resolve({ data: estimateBody, error: undefined })
    expect(await pending).toBe(false)
    expect(cost.estimate.value.kind).toBe('idle')
  })

  it('estimates one build on the chosen builder', async () => {
    const buildEstimate = {
      projectId: 'project-1',
      input: { builderBinding: 'buildkit-primary-v1', from: '2026-10-12T12:00:00.000Z', maxDurationMilliseconds: 600_000 },
      until: '2026-10-12T12:10:00.000Z',
      lines: [],
      total: { amount: '0.200000', currency: 'CREDITS' },
    }
    vi.mocked(estimateProjectBuildCost).mockResolvedValue({ data: buildEstimate, error: undefined } as never)
    const build = useProjectBuildCostEstimate(ref<string | null>('project-1'))
    expect(await build.run(buildEstimate.input)).toBe(true)
    expect(estimateProjectBuildCost).toHaveBeenCalledWith({ path: { projectId: 'project-1' }, body: buildEstimate.input })
    expect(build.estimate.value.kind).toBe('success')
  })
})

describe('creditHoldUnknownStatus', () => {
  it('shows released-unknown copy after expiry', () => {
    const status = holdStatus({
      id: 'hold-1',
      projectId: 'project-1',
      requestId: 'req-1',
      businessKey: 'metered-usage',
      ownerClientId: 'environment',
      amount: '10.000000',
      remaining: '0.000000',
      state: 'released',
      measurementUnknown: true,
      createdAt: '2026-10-12T10:00:00.000Z',
      unknownSince: '2026-10-12T10:00:00.000Z',
      reconciliationDueAt: '2026-10-12T11:00:00.000Z',
      expiredUnknownAt: '2026-10-12T11:00:00.000Z',
    })
    expect(status.kind).toBe('released')
    expect(status.message).toBe('待核实，预占已释放')
  })

  it('keeps pending unknown before expiry', () => {
    const status = holdStatus({
      id: 'hold-2',
      projectId: 'project-1',
      requestId: 'req-2',
      businessKey: 'metered-usage',
      ownerClientId: 'environment',
      amount: '10.000000',
      remaining: '10.000000',
      state: 'unknown',
      measurementUnknown: true,
      createdAt: '2026-10-12T10:00:00.000Z',
      unknownSince: '2026-10-12T10:00:00.000Z',
      reconciliationDueAt: '2026-10-12T11:00:00.000Z',
    })
    expect(status.kind).toBe('pending')
    expect(status.message).toBe('用量未知待核实')
  })
})
