import { onScopeDispose, ref, watch, type Ref } from 'vue'
import {
  estimateProjectResourceCost,
  type ResourceCostEstimateRequestSchema,
  type ResourceCostEstimateSchema,
  type ResourceCostEstimateSchemaResourceChargeLine,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'

export type ResourceCostEstimateInput = ResourceCostEstimateRequestSchema
export type ResourceCostEstimate = ResourceCostEstimateSchema
export type ResourceCostEstimateLine = ResourceCostEstimateSchemaResourceChargeLine

function diagnostic(error: unknown, fallbackCode: string, fallbackMessage: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  const code = problem?.diagnosticCode ?? fallbackCode
  const detail = problem?.detail ?? fallbackMessage
  return makeDiagnostic(code, estimateDiagnosticMessage(code, detail), problem?.retryable ?? true)
}

/** Map costing / catalog / quote diagnostics to explicit UI copy (never "free" / zero). */
export function estimateDiagnosticMessage(code: string, fallback: string): string {
  switch (code) {
    case 'LW_RESOURCE_RATE_UNCONFIGURED':
      return '缺少适用费率：当前规格或区间没有可计价的费率，估价失败，不会按免费处理。'
    case 'LW_RESOURCE_RATE_CURRENCY_CONFLICT':
      return '费率币种冲突：无法形成一致的 CREDITS 估价。'
    case 'LW_RESOURCE_RATE_OVERLAP':
      return '费率区间重叠或覆盖缺口：无法给出可信估价。'
    case 'LW_RESOURCE_GPU_CATALOG_MISSING':
      return '缺少 GPU 目录项：请求的 GPU 类别不在目录中，估价失败。'
    case 'LW_RESOURCE_GPU_CATALOG_AMBIGUOUS':
      return 'GPU 目录存在歧义：无法确定计价类别，估价失败。'
    case 'LW_RESOURCE_GPU_CATALOG_INVALID':
      return 'GPU 目录无效：无法完成估价。'
    case 'LW_RESOURCE_ESTIMATE_CHANGED':
      return '报价已过期或规格/费率已变化：请重新估价后再继续，不能使用旧报价。'
    case 'LW_CREDITS_SCOPE_DENIED':
      return '无权读取该项目的 Credits / 估价。'
    default:
      return fallback
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

function isMoney(value: unknown): value is { amount: string; currency: string } {
  return isRecord(value) && typeof value.amount === 'string' && typeof value.currency === 'string'
}

function isLine(value: unknown): value is ResourceCostEstimateLine {
  if (!isRecord(value)) return false
  const divisor = value.quantityDivisor
  return (
    isMoney(value.amount)
    && typeof value.quantity === 'number'
    && Number.isSafeInteger(value.quantity)
    && (divisor === undefined || divisor === 1 || divisor === 1000)
    && typeof value.rateId === 'string'
    && typeof value.rateRevision === 'number'
    && Number.isSafeInteger(value.rateRevision)
    && typeof value.unit === 'string'
    && isMoney(value.unitPrice)
    && typeof value.unitQuantity === 'number'
    && Number.isSafeInteger(value.unitQuantity)
  )
}

function asEstimate(value: unknown): ResourceCostEstimate {
  if (
    !isRecord(value)
    || typeof value.projectId !== 'string'
    || !isRecord(value.input)
    || !Array.isArray(value.lines)
    || !isMoney(value.total)
  ) {
    throw new Error('资源估价返回了无法识别的数据。')
  }
  for (const line of value.lines) {
    if (!isLine(line)) throw new Error('资源估价明细返回了无法识别的数据。')
  }
  return value as ResourceCostEstimate
}

/** Display quantity using server quantityDivisor. Do not recompute monetary totals. */
export function formatEstimateQuantity(
  line: Pick<ResourceCostEstimateLine, 'quantity' | 'quantityDivisor'>,
): string {
  const divisor = line.quantityDivisor ?? 1
  if (divisor === 1) return `${line.quantity}（除数 1 · 整秒）`
  if (line.quantity % divisor === 0) {
    return `${line.quantity / divisor}（quantity=${line.quantity} ÷ ${divisor}）`
  }
  return `${line.quantity}/${divisor}（quantity=${line.quantity} ÷ ${divisor}）`
}

export function billingUnitLabel(unit: string): string {
  switch (unit) {
    case 'cpu_millicore_second':
      return 'CPU millicore·秒'
    case 'memory_byte_second':
      return '内存 byte·秒'
    case 'storage_byte_second':
      return '存储 byte·秒'
    case 'gpu_unit_second':
      return 'GPU unit·秒'
    default:
      return unit
  }
}

export function hoursFromNowInterval(hours: number): { from: string; until: string } {
  const from = new Date()
  from.setMilliseconds(0)
  const until = new Date(from.getTime() + Math.max(1, hours) * 3600_000)
  return { from: from.toISOString(), until: until.toISOString() }
}

export function useProjectResourceCostEstimate(projectId: Ref<string | null>) {
  const estimate = ref<AsyncState<ResourceCostEstimate>>({ kind: 'idle' })
  const acting = ref(false)
  let requestGeneration = 0
  let disposed = false

  onScopeDispose(() => {
    disposed = true
  })

  function current(id: string, generation: number) {
    return !disposed && projectId.value === id && generation === requestGeneration
  }

  function reset() {
    requestGeneration += 1
    estimate.value = { kind: 'idle' }
    acting.value = false
  }

  async function run(input: ResourceCostEstimateInput): Promise<boolean> {
    const id = projectId.value
    if (!id || acting.value) return false
    const generation = ++requestGeneration
    acting.value = true
    estimate.value = { kind: 'loading', message: '正在向服务端请求资源估价…' }
    try {
      const result = await estimateProjectResourceCost({
        path: { projectId: id },
        body: input,
      })
      if (!current(id, generation)) return false
      if (result.error) throw result.error
      estimate.value = { kind: 'success', data: asEstimate(result.data) }
      return true
    } catch (error) {
      if (!current(id, generation)) return false
      estimate.value = {
        kind: 'error',
        diagnostic: diagnostic(error, 'RESOURCE_COST_ESTIMATE_FAILED', '无法完成资源估价。'),
      }
      return false
    } finally {
      if (generation === requestGeneration) acting.value = false
    }
  }

  watch(projectId, () => {
    reset()
  })

  return {
    estimate,
    acting,
    run,
    reset,
  }
}
