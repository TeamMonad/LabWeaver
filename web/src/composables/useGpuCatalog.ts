import { reactive, ref } from 'vue'
import {
  createResourceGpuCatalogEntry,
  listResourceGpuCatalog,
} from '@/generated/contracts'
import type {
  GpuAllocationMode,
  GpuCatalogEntrySchema,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, newUuidV7 } from '@/utils/format'

export type GpuCatalogState =
  | { kind: 'idle' }
  | { kind: 'loading' }
  | { kind: 'ready' }
  | { kind: 'submitting' }
  | { kind: 'error'; diagnostic: DiagnosticViewModel }

export interface CreateGpuCatalogEntryInput {
  /** Catalog class callers request, e.g. `nvidia-a10`. */
  class: string
  /** Allocation mode the class resolves to; callers never override it per request. */
  mode: GpuAllocationMode
  /** Capacity provider that observes and reserves this class. */
  providerBinding: string
  /** Capacity units the entry contributes to the class. */
  capacityUnits: number
  /** Opaque provider mapping for the allocation, e.g. a device-plugin resource name. */
  allocationBinding: string
  /** Monotonic revision the new entry declares; the server rejects a stale one. */
  revision: number
}

/**
 * Administrator GPU class catalog over the Resource Service.
 *
 * The class identity and revision are client-supplied, so the composable mints
 * a fresh aggregate id per create and always re-reads the server catalog after
 * a successful write. A failed write keeps the rendered catalog intact.
 */
export function useGpuCatalog() {
  const entries = ref<GpuCatalogEntrySchema[]>([])
  const state = ref<GpuCatalogState>({ kind: 'idle' })

  function failure(error: unknown, fallbackCode: string, fallbackMessage: string): void {
    const problem = extractProblemDetails(error)
    state.value = {
      kind: 'error',
      diagnostic: makeDiagnostic(
        problem?.diagnosticCode ?? fallbackCode,
        problem?.detail ?? fallbackMessage,
        problem?.retryable ?? true,
      ),
    }
  }

  async function load(): Promise<void> {
    state.value = { kind: 'loading' }
    const result = await listResourceGpuCatalog()
    if (result.error) {
      failure(result.error, 'GPU_CATALOG_LOAD_FAILED', '加载 GPU 目录失败。')
      return
    }
    entries.value = result.data
    state.value = { kind: 'ready' }
  }

  async function create(input: CreateGpuCatalogEntryInput): Promise<boolean> {
    state.value = { kind: 'submitting' }
    const result = await createResourceGpuCatalogEntry({
      headers: { 'Idempotency-Key': idempotencyKey() },
      body: {
        id: newUuidV7(),
        class: input.class,
        mode: input.mode,
        providerBinding: input.providerBinding,
        capacityUnits: input.capacityUnits,
        allocationBinding: input.allocationBinding,
        revision: input.revision,
        active: true,
      },
    })
    if (result.error) {
      failure(result.error, 'GPU_CATALOG_CREATE_FAILED', '创建 GPU 目录项失败。')
      return false
    }
    await load()
    return true
  }

  function clearDiagnostic(): void {
    if (state.value.kind !== 'error') return
    state.value = entries.value.length > 0 ? { kind: 'ready' } : { kind: 'idle' }
  }

  return reactive({ entries, state, load, create, clearDiagnostic })
}
