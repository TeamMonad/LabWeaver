import { computed, onScopeDispose, reactive, ref, watch, type Ref } from 'vue'
import type {
  EnvironmentSummary,
  GpuAllocationMode,
} from '@/generated/contracts'
import { apiClient } from '@/api/client'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { fetchEnvironmentTemplateReleases } from '@/composables/useEnvironmentTemplateReleases'
import {
  fetchProjectWorkEnvironments,
  ProjectWorkEnvironmentPaginationError,
} from '@/composables/useProjectWorkEnvironments'

/** Publicly renderable GPU catalog data. Provider bindings stay out of the view model. */
export interface ResourceGpuCatalogOption {
  id: string
  class: string
  mode: GpuAllocationMode
  capacityUnits: number
  revision: number
  active: boolean
}

export interface ResourceRateOption {
  id: string
  revision: number
  unit: 'cpu_millicore_second' | 'memory_byte_second' | 'storage_byte_second' | 'gpu_unit_second'
  unitQuantity: number
  gpuClass?: string | null
  gpuMode?: GpuAllocationMode | null
  unitPrice: { currency: string; amount: string }
  effectiveFrom: string
  effectiveUntil?: string | null
}

export interface ResourceGpuRateSelection {
  rate: ResourceRateOption | null
  ambiguous: boolean
}

/**
 * Selects the rate that is authoritative at one instant for a GPU catalog entry.
 *
 * The API returns immutable historical revisions, so matching only by class and
 * mode can display a future or expired price.  A valid interval is selected
 * first, then every interval must be the sole match. Any overlap is ambiguous,
 * regardless of revision, so the UI cannot choose a made-up estimate.
 */
export function selectCurrentGpuRateSelection(
  rates: ResourceRateOption[],
  entry: Pick<ResourceGpuCatalogOption, 'class' | 'mode'>,
  at: Date = new Date(),
): ResourceGpuRateSelection {
  const instant = at.getTime()
  if (!Number.isFinite(instant)) return { rate: null, ambiguous: false }

  const matching = rates.filter((rate) => {
    const effectiveFrom = Date.parse(rate.effectiveFrom)
    const effectiveUntil = rate.effectiveUntil ? Date.parse(rate.effectiveUntil) : Number.POSITIVE_INFINITY
    return (
      rate.unit === 'gpu_unit_second' &&
      rate.gpuClass === entry.class &&
      rate.gpuMode === entry.mode &&
      Number.isFinite(effectiveFrom) &&
      effectiveFrom <= instant &&
      effectiveUntil > instant
    )
  })
  if (matching.length === 0) return { rate: null, ambiguous: false }

  return matching.length === 1
    ? { rate: matching[0], ambiguous: false }
    : { rate: null, ambiguous: true }
}

export function selectCurrentGpuRate(
  rates: ResourceRateOption[],
  entry: Pick<ResourceGpuCatalogOption, 'class' | 'mode'>,
  at: Date = new Date(),
): ResourceRateOption | null {
  return selectCurrentGpuRateSelection(rates, entry, at).rate
}

/** A release option carries only the identity Resource needs to bind a Work request. */
export interface ProjectReleaseOption {
  id: string
  version: number
  runtimeKind: 'container' | 'virtual_machine'
  label: string
  source: 'control'
}

function diagnostic(error: unknown, fallbackCode: string, fallbackMessage: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackMessage, problem?.retryable ?? true)
}

function stateFromItems<T>(items: T[]): AsyncState<T[]> {
  return items.length > 0 ? { kind: 'success', data: items } : { kind: 'empty' }
}

function isCatalogOption(value: unknown): value is ResourceGpuCatalogOption {
  if (!value || typeof value !== 'object') return false
  const item = value as Record<string, unknown>
  return (
    typeof item.id === 'string' &&
    typeof item.class === 'string' &&
    ['exclusive', 'container_time_slice', 'vm_vgpu'].includes(String(item.mode)) &&
    Number.isSafeInteger(item.capacityUnits) &&
    Number(item.capacityUnits) > 0 &&
    Number.isSafeInteger(item.revision) &&
    Number(item.revision) > 0 &&
    typeof item.active === 'boolean'
  )
}

function isRateOption(value: unknown): value is ResourceRateOption {
  if (!value || typeof value !== 'object') return false
  const item = value as Record<string, unknown>
  const price = item.unitPrice
  const unit = String(item.unit)
  const gpuMode = item.gpuMode
  const hasGpuDimension = typeof item.gpuClass === 'string' && gpuMode !== null && gpuMode !== undefined
  const validGpuMode = gpuMode === null || gpuMode === undefined || ['exclusive', 'container_time_slice', 'vm_vgpu'].includes(String(gpuMode))
  const effectiveUntil = item.effectiveUntil
  return (
    typeof item.id === 'string' &&
    Number.isSafeInteger(item.revision) &&
    Number(item.revision) > 0 &&
    ['cpu_millicore_second', 'memory_byte_second', 'storage_byte_second', 'gpu_unit_second'].includes(unit) &&
    validGpuMode &&
    ((unit === 'gpu_unit_second' && hasGpuDimension) || (unit !== 'gpu_unit_second' && !hasGpuDimension)) &&
    Number.isSafeInteger(item.unitQuantity) &&
    Number(item.unitQuantity) > 0 &&
    typeof price === 'object' &&
    price !== null &&
    typeof (price as Record<string, unknown>).currency === 'string' &&
    typeof (price as Record<string, unknown>).amount === 'string' &&
    typeof item.effectiveFrom === 'string' &&
    Number.isFinite(Date.parse(item.effectiveFrom)) &&
    (effectiveUntil === null || effectiveUntil === undefined || (typeof effectiveUntil === 'string' && Number.isFinite(Date.parse(effectiveUntil))))
  )
}

export function useProjectResourceOptions(
  projectId: Ref<string | null>,
  courseId: Ref<string | null | undefined>,
) {
  const environments = ref<AsyncState<EnvironmentSummary[]>>({ kind: 'idle' })
  const releases = ref<AsyncState<ProjectReleaseOption[]>>({ kind: 'idle' })
  const catalog = ref<AsyncState<ResourceGpuCatalogOption[]>>({ kind: 'idle' })
  const rates = ref<AsyncState<ResourceRateOption[]>>({ kind: 'idle' })
  const outcome = ref<DiagnosticViewModel | null>(null)
  let loadGeneration = 0

  async function load() {
    const id = projectId.value
    const generation = ++loadGeneration
    outcome.value = null
    if (!id) {
      environments.value = { kind: 'idle' }
      releases.value = { kind: 'idle' }
      catalog.value = { kind: 'idle' }
      rates.value = { kind: 'idle' }
      return
    }

    environments.value = { kind: 'loading', message: '加载 Work 环境…' }
    releases.value = { kind: 'loading', message: '加载已发布版本…' }
    catalog.value = { kind: 'loading', message: '加载 GPU 目录…' }
    rates.value = { kind: 'loading', message: '加载资源费率…' }

    let environmentItems: EnvironmentSummary[] = []
    try {
      environmentItems = await fetchProjectWorkEnvironments(id)
    } catch (error) {
      if (generation !== loadGeneration) return
      const paginationError = error instanceof ProjectWorkEnvironmentPaginationError
      environments.value = {
        kind: 'error',
        diagnostic: paginationError
          ? makeDiagnostic(error.diagnosticCode, error.message, false)
          : diagnostic(error, 'PROJECT_ENVIRONMENTS_LOAD_FAILED', '加载 Work 环境失败'),
      }
    }
    if (generation !== loadGeneration) return
    if (environments.value.kind !== 'error') {
      environments.value = stateFromItems(environmentItems)
    }

    const releasePromise = fetchEnvironmentTemplateReleases(id, courseId.value ?? undefined)
    const [releaseResult, catalogResult, rateResult] = await Promise.all([
      releasePromise,
      apiClient.get<unknown[], unknown>({ url: '/api/v1/resource/gpu-catalog' }),
      apiClient.get<unknown[], unknown>({ url: '/api/v1/resource/rates' }),
    ])
    if (generation !== loadGeneration) return

    if (releaseResult.error) {
      releases.value = { kind: 'error', diagnostic: diagnostic(releaseResult.error, 'PROJECT_RELEASES_LOAD_FAILED', '加载已发布版本失败') }
    } else {
      const items = releaseResult.items.filter((item) => !item.withdrawal)
      releases.value = stateFromItems(
        items.map((item) => ({
          id: item.id,
          version: item.version,
          runtimeKind: item.runtimeKind,
          label: `Release ${item.id} · v${item.version}`,
          source: 'control' as const,
        })),
      )
    }

    if (catalogResult.error) {
      catalog.value = { kind: 'error', diagnostic: diagnostic(catalogResult.error, 'GPU_CATALOG_LOAD_FAILED', '加载 GPU 目录失败') }
    } else if (!Array.isArray(catalogResult.data)) {
      catalog.value = { kind: 'error', diagnostic: makeDiagnostic('GPU_CATALOG_INVALID', 'Resource 返回了无法识别的 GPU 目录响应。', false) }
    } else {
      // Validate the complete response before removing inactive historical
      // revisions.  Inactive entries are valid server data and must not turn a
      // normal catalog response into a client-side error.
      const validItems = catalogResult.data.filter(isCatalogOption)
      if (validItems.length !== catalogResult.data.length) {
        catalog.value = { kind: 'error', diagnostic: makeDiagnostic('GPU_CATALOG_INVALID', 'Resource 返回了无法识别的 GPU 目录项。', false) }
      } else {
        catalog.value = stateFromItems(validItems.filter((item) => item.active))
      }
    }

    if (rateResult.error) {
      rates.value = { kind: 'error', diagnostic: diagnostic(rateResult.error, 'RESOURCE_RATES_LOAD_FAILED', '加载资源费率失败') }
    } else if (!Array.isArray(rateResult.data)) {
      rates.value = { kind: 'error', diagnostic: makeDiagnostic('RESOURCE_RATES_INVALID', 'Resource 返回了无法识别的费率响应。', false) }
    } else {
      const items = rateResult.data.filter(isRateOption)
      rates.value = items.length === rateResult.data.length
        ? stateFromItems(items)
        : { kind: 'error', diagnostic: makeDiagnostic('RESOURCE_RATES_INVALID', 'Resource 返回了无法识别的费率。', false) }
    }
  }

  const gpuRateSelection = computed(() => (entry: ResourceGpuCatalogOption): ResourceGpuRateSelection => {
    if (rates.value.kind !== 'success') return { rate: null, ambiguous: false }
    return selectCurrentGpuRateSelection(rates.value.data, entry)
  })
  const gpuRate = computed(() => (entry: ResourceGpuCatalogOption) => {
    return gpuRateSelection.value(entry).rate
  })

  watch([projectId, courseId], () => void load(), { immediate: true })
  onScopeDispose(() => { loadGeneration += 1 })

  return reactive({ environments, releases, catalog, rates, outcome, gpuRate, gpuRateSelection, load })
}
