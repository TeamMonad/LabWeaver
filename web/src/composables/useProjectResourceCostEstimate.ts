import { onScopeDispose, ref, watch, type Ref } from 'vue'
import {
  estimateProjectBuildCost,
  estimateProjectResourceCost,
  type BuildCostEstimateRequestSchema,
  type BuildCostEstimateSchema,
  type ResourceBillingUnit,
  type ResourceCostEstimateRequestSchema,
  type ResourceCostEstimateSchema,
  type ResourceCostEstimateSchemaResourceChargeLine,
} from '@/generated/contracts'
import { diagnosticFromError, type AsyncState } from '@/types/async'

export type ResourceCostEstimateInput = ResourceCostEstimateRequestSchema
export type ResourceCostEstimate = ResourceCostEstimateSchema
export type ResourceCostEstimateLine = ResourceCostEstimateSchemaResourceChargeLine
export type BuildCostEstimateInput = BuildCostEstimateRequestSchema
export type BuildCostEstimate = BuildCostEstimateSchema

const unitDisplay: Record<ResourceBillingUnit, { label: string; scale: number; quantityUnit: string }> = {
  cpu_millicore_second: { label: 'CPU', scale: 1000 * 3600, quantityUnit: '核·小时' },
  memory_byte_second: { label: '内存', scale: 1024 ** 3 * 3600, quantityUnit: 'GiB·小时' },
  storage_byte_second: { label: '存储', scale: 1024 ** 3 * 3600, quantityUnit: 'GiB·小时' },
  gpu_unit_second: { label: 'GPU', scale: 3600, quantityUnit: '分配单位·小时' },
  build_second: { label: '镜像构建', scale: 1, quantityUnit: '秒' },
  llm_input_token: { label: '模型输入 token', scale: 1, quantityUnit: 'token' },
  llm_output_token: { label: '模型输出 token', scale: 1, quantityUnit: 'token' },
  llm_cache_read_token: { label: '模型缓存读取 token', scale: 1, quantityUnit: 'token' },
  llm_cache_write_token: { label: '模型缓存写入 token', scale: 1, quantityUnit: 'token' },
}

export function billingUnitLabel(unit: string): string {
  return unitDisplay[unit as ResourceBillingUnit]?.label ?? unit
}

/**
 * Human-readable quantity of one charge line, derived from the server's
 * quantity and quantityDivisor. Display only: money always comes from the
 * server line amount and total.
 */
export function formatEstimateQuantity(
  line: Pick<ResourceCostEstimateLine, 'quantity' | 'quantityDivisor' | 'unit'>,
): string {
  const display = unitDisplay[line.unit]
  const divisor = line.quantityDivisor ?? 1
  if (!display) return `${line.quantity}${divisor === 1 ? '' : ` ÷ ${divisor}`}`
  const value = line.quantity / divisor / display.scale
  const rounded = Number(value.toFixed(4))
  const approximate = rounded !== value ? '约 ' : ''
  return `${approximate}${rounded.toLocaleString('zh-CN', { maximumFractionDigits: 4 })} ${display.quantityUnit}`
}

export function hoursFromNowInterval(hours: number): { from: string; until: string } {
  const from = new Date()
  from.setMilliseconds(0)
  const until = new Date(from.getTime() + Math.max(1, hours) * 3600_000)
  return { from: from.toISOString(), until: until.toISOString() }
}

/** One server estimate request with stale-response protection per project. */
function useEstimateRequest<Input, Output>(
  projectId: Ref<string | null>,
  request: (id: string, input: Input) => Promise<{ data?: Output; error?: unknown }>,
  failure: { code: string; message: string },
) {
  const estimate = ref<AsyncState<Output>>({ kind: 'idle' }) as Ref<AsyncState<Output>>
  const acting = ref(false)
  let generation = 0
  let disposed = false
  onScopeDispose(() => { disposed = true })

  function reset() {
    generation += 1
    estimate.value = { kind: 'idle' }
    acting.value = false
  }

  async function run(input: Input): Promise<boolean> {
    const id = projectId.value
    if (!id || acting.value) return false
    const current = ++generation
    const stale = () => disposed || projectId.value !== id || current !== generation
    acting.value = true
    estimate.value = { kind: 'loading', message: '正在估价…' }
    try {
      const result = await request(id, input)
      if (stale()) return false
      if (result.error || !result.data) throw result.error
      estimate.value = { kind: 'success', data: result.data }
      return true
    } catch (error) {
      if (stale()) return false
      estimate.value = { kind: 'error', diagnostic: diagnosticFromError(error, failure.code, failure.message) }
      return false
    } finally {
      if (current === generation) acting.value = false
    }
  }

  watch(projectId, reset)
  return { estimate, acting, run, reset }
}

/** Compute/storage estimate for a resource request (estimateProjectResourceCost). No funds or capacity side effects. */
export function useProjectResourceCostEstimate(projectId: Ref<string | null>) {
  return useEstimateRequest<ResourceCostEstimateInput, ResourceCostEstimate>(
    projectId,
    (id, body) => estimateProjectResourceCost({ path: { projectId: id }, body }),
    { code: 'RESOURCE_COST_ESTIMATE_FAILED', message: '无法完成资源估价。' },
  )
}

/** Upper-bound estimate for one image build on a configured builder (estimateProjectBuildCost). */
export function useProjectBuildCostEstimate(projectId: Ref<string | null>) {
  return useEstimateRequest<BuildCostEstimateInput, BuildCostEstimate>(
    projectId,
    (id, body) => estimateProjectBuildCost({ path: { projectId: id }, body }),
    { code: 'BUILD_COST_ESTIMATE_FAILED', message: '无法完成构建费用估价。' },
  )
}
