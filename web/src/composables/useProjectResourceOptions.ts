import { computed, onScopeDispose, reactive, ref, watch, type Ref } from 'vue'
import type {
  EnvironmentSummary,
  GpuAllocationMode,
  ResourceBillingUnit,
  ResourceRateSchema,
} from '@/generated/contracts'
import { fetchAllGpuCatalogEntries, fetchAllResourceRates } from '@/utils/resourceLists'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { fetchEnvironmentTemplateReleases } from '@/composables/useEnvironmentTemplateReleases'
import { formatTimestamp } from '@/utils/format'
import { isWorkTemplate, workTemplateTitle } from '@/utils/workTemplate'
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

/** A Resource rate version as returned by the generated contract. */
export type ResourceRateOption = ResourceRateSchema

/**
 * Units that price a resource request (compute, storage, GPU). Build and model
 * token rates are valid list entries too, but they never price a request, so
 * the request page keeps only these.
 */
const RESOURCE_REQUEST_UNITS: readonly ResourceBillingUnit[] = ['cpu_millicore_second', 'memory_byte_second', 'storage_byte_second', 'gpu_unit_second']

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

/** A release option carries only the identity Resource needs to bind an environment request. */
export interface ProjectReleaseOption {
  id: string
  version: number
  runtimeKind: 'container' | 'virtual_machine'
  /** `experiment` only appears for a course project; it creates the student's course experiment environment. */
  purpose: 'work' | 'experiment'
  label: string
  source: 'control'
}

function diagnostic(error: unknown, fallbackCode: string, fallbackMessage: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackMessage, problem?.retryable ?? true, problem)
}

function stateFromItems<T>(items: T[]): AsyncState<T[]> {
  return items.length > 0 ? { kind: 'success', data: items } : { kind: 'empty' }
}

type Settled<T> = { ok: true; value: T } | { ok: false; error: unknown }

function settle<T>(promise: Promise<T>): Promise<Settled<T>> {
  return promise.then((value) => ({ ok: true as const, value }), (error: unknown) => ({ ok: false as const, error }))
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
      settle(fetchAllGpuCatalogEntries()),
      settle(fetchAllResourceRates()),
    ])
    if (generation !== loadGeneration) return

    if (releaseResult.error) {
      releases.value = { kind: 'error', diagnostic: diagnostic(releaseResult.error, 'PROJECT_RELEASES_LOAD_FAILED', '加载已发布版本失败') }
    } else {
      // Work releases create Work. A course project also offers the course
      // experiment releases the server returned to this user, so a student
      // can request the resources for their own experiment environment.
      const projectCourseId = courseId.value ?? null
      const items = releaseResult.items.filter((item) => isWorkTemplate(item)
        || (projectCourseId !== null && item.purpose === 'experiment' && !item.withdrawal))
      releases.value = stateFromItems(
        items.map((item) => ({
          id: item.id,
          version: item.version,
          runtimeKind: item.runtimeKind,
          purpose: item.purpose,
          label: `${item.purpose === 'experiment' ? '课程实验 · ' : ''}${workTemplateTitle(item)} · 发布于 ${formatTimestamp(item.publishedAt)}`,
          source: 'control' as const,
        })),
      )
    }

    if (!catalogResult.ok) {
      catalog.value = { kind: 'error', diagnostic: diagnostic(catalogResult.error, 'GPU_CATALOG_LOAD_FAILED', '加载 GPU 目录失败') }
    } else {
      // Validate the complete response before removing inactive historical
      // revisions.  Inactive entries are valid server data and must not turn a
      // normal catalog response into a client-side error.
      const entries: unknown[] = catalogResult.value
      const validItems = entries.filter(isCatalogOption)
      if (validItems.length !== entries.length) {
        catalog.value = { kind: 'error', diagnostic: makeDiagnostic('GPU_CATALOG_INVALID', 'Resource 返回了无法识别的 GPU 目录项。', false) }
      } else {
        catalog.value = stateFromItems(validItems.filter((item) => item.active))
      }
    }

    if (!rateResult.ok) {
      rates.value = { kind: 'error', diagnostic: diagnostic(rateResult.error, 'RESOURCE_RATES_LOAD_FAILED', '加载资源费率失败') }
    } else {
      rates.value = stateFromItems(rateResult.value.filter((rate) => RESOURCE_REQUEST_UNITS.includes(rate.unit)))
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
