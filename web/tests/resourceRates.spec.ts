import { describe, expect, it } from 'vitest'
import type { ResourceRateSchema } from '@/generated/contracts'
import {
  modelRateReadiness,
  modelRateRequests,
  rateCurrencyError,
  rateDimensionError,
  supersededRate,
  tokensCoveredByCap,
  validRateBinding,
} from '@/utils/resourceRates'

function rate(overrides: Partial<ResourceRateSchema>): ResourceRateSchema {
  return {
    id: 'rate', revision: 1, unit: 'llm_input_token', unitQuantity: 1_000_000, modelBinding: 'claude-sonnet-4',
    maxPreauthorization: '2.000000', unitPrice: { currency: 'CREDITS', amount: '3.000000' },
    effectiveFrom: '2026-01-01T00:00:00Z', effectiveUntil: null, ...overrides,
  } as ResourceRateSchema
}

const now = Date.parse('2026-11-04T00:00:00Z')
const fourModelRates = [
  rate({ id: 'in', unit: 'llm_input_token' }),
  rate({ id: 'out', unit: 'llm_output_token', unitPrice: { currency: 'CREDITS', amount: '15.000000' } }),
  rate({ id: 'cr', unit: 'llm_cache_read_token', unitPrice: { currency: 'CREDITS', amount: '0.300000' } }),
  rate({ id: 'cw', unit: 'llm_cache_write_token', unitPrice: { currency: 'CREDITS', amount: '3.750000' } }),
]

describe('rate dimension rule (LW_RESOURCE_RATE_DIMENSION_INVALID semantics)', () => {
  it('requires a builder binding exactly for build seconds', () => {
    expect(rateDimensionError({ unit: 'build_second', builderBinding: 'buildkit-primary-v1' })).toBeNull()
    expect(rateDimensionError({ unit: 'build_second' })).toContain('必须填写构建器标识')
    expect(rateDimensionError({ unit: 'build_second', builderBinding: null })).toContain('必须填写构建器标识')
    expect(rateDimensionError({ unit: 'cpu_millicore_second', builderBinding: 'buildkit-primary-v1' })).toContain('只有构建费率')
    expect(rateDimensionError({ unit: 'llm_input_token', builderBinding: 'b', modelBinding: 'm', maxPreauthorization: '1' })).toContain('只有构建费率')
  })

  it('applies the Resource binding text rule: non-blank, no ASCII whitespace, byte limit', () => {
    for (const value of ['', '   ', 'buildkit primary', 'buildkit\tprimary', 'a\nb']) {
      expect(rateDimensionError({ unit: 'build_second', builderBinding: value })).toContain('不能含空格')
    }
    expect(validRateBinding('a'.repeat(128), 128)).toBe(true)
    expect(validRateBinding('a'.repeat(129), 128)).toBe(false)
    // 43 three-byte characters are 129 UTF-8 bytes although only 43 UTF-16 code units.
    expect(validRateBinding('构'.repeat(42), 128)).toBe(true)
    expect(validRateBinding('构'.repeat(43), 128)).toBe(false)
    expect(validRateBinding('m'.repeat(256), 256)).toBe(true)
    expect(validRateBinding('m'.repeat(257), 256)).toBe(false)
  })

  it('requires the GPU class and mode together exactly for GPU units', () => {
    expect(rateDimensionError({ unit: 'gpu_unit_second', gpuClass: 'nvidia-a100', gpuMode: 'exclusive' })).toBeNull()
    expect(rateDimensionError({ unit: 'gpu_unit_second' })).toContain('GPU 费率必须选择')
    expect(rateDimensionError({ unit: 'gpu_unit_second', gpuClass: 'nvidia-a100' })).toContain('同时选择')
    expect(rateDimensionError({ unit: 'gpu_unit_second', gpuMode: 'exclusive' })).toContain('同时选择')
    expect(rateDimensionError({ unit: 'memory_byte_second', gpuClass: 'nvidia-a100', gpuMode: 'exclusive' })).toContain('只有 GPU 费率')
  })

  it('requires a model binding and a positive per-call cap exactly for the four token units', () => {
    for (const unit of ['llm_input_token', 'llm_output_token', 'llm_cache_read_token', 'llm_cache_write_token'] as const) {
      expect(rateDimensionError({ unit, modelBinding: 'claude-sonnet-4', maxPreauthorization: '2.5' })).toBeNull()
      expect(rateDimensionError({ unit, maxPreauthorization: '2.5' })).toContain('必须填写模型标识')
      expect(rateDimensionError({ unit, modelBinding: 'claude-sonnet-4' })).toContain('大于 0')
      for (const cap of ['0', '0.000000', '-1', '1.0000001', 'abc']) {
        expect(rateDimensionError({ unit, modelBinding: 'claude-sonnet-4', maxPreauthorization: cap })).toContain('大于 0')
      }
      expect(rateDimensionError({ unit, modelBinding: 'claude sonnet', maxPreauthorization: '1' })).toContain('模型标识不能为空')
    }
    expect(rateDimensionError({ unit: 'storage_byte_second', modelBinding: 'claude-sonnet-4' })).toContain('只有模型 token 费率')
    expect(rateDimensionError({ unit: 'cpu_millicore_second', maxPreauthorization: '1' })).toContain('只有模型 token 费率')
  })

  it('keeps build and model rates in CREDITS without currency conversion', () => {
    expect(rateCurrencyError('build_second', 'CREDITS')).toBeNull()
    expect(rateCurrencyError('llm_output_token', 'CREDITS')).toBeNull()
    expect(rateCurrencyError('build_second', 'USD')).toContain('只能使用 CREDITS')
    expect(rateCurrencyError('llm_input_token', 'USD')).toContain('只能使用 CREDITS')
    expect(rateCurrencyError('cpu_millicore_second', 'USD')).toBeNull()
    expect(rateCurrencyError('cpu_millicore_second', 'bad currency')).toContain('币种只能')
  })
})

describe('model rate readiness', () => {
  it('is ready only with all four current dimensions at one cap', () => {
    const ready = modelRateReadiness(fourModelRates, 'claude-sonnet-4', now)
    expect(ready).toEqual(expect.objectContaining({ kind: 'ready', maxPreauthorization: '2.000000' }))
    expect(modelRateReadiness(fourModelRates.slice(0, 3), 'claude-sonnet-4', now)).toEqual({ kind: 'missing', missing: ['llm_cache_write_token'] })
    expect(modelRateReadiness(fourModelRates, 'other-model', now)).toEqual({ kind: 'missing', missing: ['llm_input_token', 'llm_output_token', 'llm_cache_read_token', 'llm_cache_write_token'] })
  })

  it('reports differing caps that Resource refuses at admission', () => {
    const mismatched = [...fourModelRates.slice(0, 3), rate({ id: 'cw', unit: 'llm_cache_write_token', maxPreauthorization: '5.000000' })]
    expect(modelRateReadiness(mismatched, 'claude-sonnet-4', now)).toEqual({ kind: 'cap_mismatch', caps: ['2.000000', '5.000000'] })
  })

  it('ignores ended and future versions and uses the latest current one', () => {
    const rates = [
      ...fourModelRates.slice(0, 3),
      rate({ id: 'cw-ended', unit: 'llm_cache_write_token', effectiveUntil: '2026-06-01T00:00:00Z' }),
      rate({ id: 'cw-future', unit: 'llm_cache_write_token', effectiveFrom: '2027-01-01T00:00:00Z' }),
    ]
    expect(modelRateReadiness(rates, 'claude-sonnet-4', now)).toEqual({ kind: 'missing', missing: ['llm_cache_write_token'] })
    const replaced = [...fourModelRates, rate({ id: 'in-2', revision: 2, unit: 'llm_input_token', effectiveFrom: '2026-10-01T00:00:00Z', maxPreauthorization: '9.000000' })]
    expect(modelRateReadiness(replaced, 'claude-sonnet-4', now).kind).toBe('cap_mismatch')
  })
})

describe('rate impact hints', () => {
  it('computes tokens covered by the cap exactly and never divides by a zero price', () => {
    expect(tokensCoveredByCap('2.000000', '15.000000')).toBe(133_333n)
    expect(tokensCoveredByCap('2', '0.000001')).toBe(2_000_000_000_000n)
    expect(tokensCoveredByCap('2', '0')).toBeNull()
  })

  it('names the open version of the same dimension that a new version closes', () => {
    const rates = [
      rate({ id: 'build-old', unit: 'build_second', modelBinding: null, maxPreauthorization: null, builderBinding: 'buildkit-a', unitQuantity: 3600, effectiveUntil: '2026-02-01T00:00:00Z' }),
      rate({ id: 'build-open', revision: 2, unit: 'build_second', modelBinding: null, maxPreauthorization: null, builderBinding: 'buildkit-a', unitQuantity: 3600, effectiveFrom: '2026-02-01T00:00:00Z' }),
      rate({ id: 'build-other', unit: 'build_second', modelBinding: null, maxPreauthorization: null, builderBinding: 'buildkit-b', unitQuantity: 3600 }),
    ]
    expect(supersededRate(rates, { unit: 'build_second', builderBinding: 'buildkit-a' })?.id).toBe('build-open')
    expect(supersededRate(rates, { unit: 'build_second', builderBinding: 'buildkit-c' })).toBeNull()
  })
})

describe('model rate requests', () => {
  const draft = {
    modelBinding: 'claude-sonnet-4',
    maxPreauthorization: '2.5',
    prices: { llm_input_token: '3', llm_output_token: '15', llm_cache_read_token: '0.3', llm_cache_write_token: '3.75' },
    effectiveFrom: '2030-01-01T00:00:00.000Z',
    effectiveUntil: null,
  }

  it('creates four CREDITS-per-million rates sharing one cap', () => {
    const result = modelRateRequests(draft)
    expect('requests' in result && result.requests).toEqual([
      ['llm_input_token', '3.000000'], ['llm_output_token', '15.000000'], ['llm_cache_read_token', '0.300000'], ['llm_cache_write_token', '3.750000'],
    ].map(([unit, amount]) => ({
      unit, unitQuantity: 1_000_000, gpuClass: null, gpuMode: null, modelBinding: 'claude-sonnet-4', maxPreauthorization: '2.500000',
      unitPrice: { currency: 'CREDITS', amount }, effectiveFrom: '2030-01-01T00:00:00.000Z', effectiveUntil: null,
    })))
  })

  it('has no default price: every dimension must be entered', () => {
    expect(modelRateRequests({ ...draft, prices: { ...draft.prices, llm_cache_read_token: '' } })).toEqual({ error: expect.stringContaining('缓存读取') })
    expect(modelRateRequests({ ...draft, maxPreauthorization: '0' })).toEqual({ error: expect.stringContaining('大于 0') })
    expect(modelRateRequests({ ...draft, modelBinding: '' })).toEqual({ error: expect.stringContaining('模型标识') })
  })
})
