import { flushPromises } from '@vue/test-utils'
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { ref } from 'vue'
import { estimateProjectResourceCost } from '@/generated/contracts'
import {
  billingUnitLabel,
  estimateDiagnosticMessage,
  formatEstimateQuantity,
  useProjectResourceCostEstimate,
} from '@/composables/useProjectResourceCostEstimate'
import { creditHoldUnknownStatus as holdStatus } from '@/composables/useProjectCredits'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
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
  })

  it('formats quantity with quantityDivisor without inventing money totals', () => {
    expect(formatEstimateQuantity({ quantity: 400, quantityDivisor: 1000 })).toContain('÷ 1000')
    expect(formatEstimateQuantity({ quantity: 8, quantityDivisor: 1 })).toContain('除数 1')
    expect(billingUnitLabel('gpu_unit_second')).toContain('GPU')
  })

  it('maps missing rate / catalog / expired quote diagnostics', () => {
    expect(estimateDiagnosticMessage('LW_RESOURCE_RATE_UNCONFIGURED', 'x')).toContain('缺少适用费率')
    expect(estimateDiagnosticMessage('LW_RESOURCE_GPU_CATALOG_MISSING', 'x')).toContain('缺少 GPU 目录')
    expect(estimateDiagnosticMessage('LW_RESOURCE_ESTIMATE_CHANGED', 'x')).toContain('过期')
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

  it('surfaces unconfigured rate as explicit error (not zero)', async () => {
    vi.mocked(estimateProjectResourceCost).mockResolvedValue({
      data: undefined,
      error: {
        diagnosticCode: 'LW_RESOURCE_RATE_UNCONFIGURED',
        detail: 'rate missing',
        status: 409,
        title: 'Conflict',
        type: 'about:blank',
      },
    } as never)

    const projectId = ref<string | null>('project-1')
    const cost = useProjectResourceCostEstimate(projectId)
    await cost.run(estimateBody.input)
    await flushPromises()

    expect(cost.estimate.value.kind).toBe('error')
    if (cost.estimate.value.kind === 'error') {
      expect(cost.estimate.value.diagnostic.code).toBe('LW_RESOURCE_RATE_UNCONFIGURED')
      expect(cost.estimate.value.diagnostic.message).toContain('缺少适用费率')
      expect(cost.estimate.value.diagnostic.message).toContain('不会按免费处理')
      expect(cost.estimate.value.diagnostic.message).not.toMatch(/0\.000000|显示为零|按免费结算/)
    }
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
