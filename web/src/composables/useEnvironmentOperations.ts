import { reactive, ref, watch, type Ref } from 'vue'
import { cancelEnvironmentOperation, listEnvironmentOperations } from '@/generated/contracts'
import type {
  EnvironmentOperationAcceptedSchema,
  EnvironmentOperationSnapshotSchema,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

export type EnvironmentOperationMutationResult =
  | { ok: true; accepted: EnvironmentOperationAcceptedSchema }
  | { ok: false; diagnostic: DiagnosticViewModel }

export function useEnvironmentOperations(environmentId: Ref<string | undefined>) {
  const operations = ref<AsyncState<EnvironmentOperationSnapshotSchema[]>>({ kind: 'idle' })
  const cancelling = ref(false)
  const cancelDiagnostic = ref<DiagnosticViewModel | null>(null)
  let loadGeneration = 0

  async function load() {
    const id = environmentId.value
    const generation = ++loadGeneration
    if (!id) {
      operations.value = { kind: 'idle' }
      return
    }
    operations.value = { kind: 'loading', message: '加载环境操作历史…' }
    const result = await listEnvironmentOperations({ path: { environmentId: id } })
    if (generation !== loadGeneration || environmentId.value !== id) return
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      operations.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'OPERATION_LIST_FAILED',
          problem?.detail ?? '加载环境操作历史失败',
          problem?.retryable ?? true,
        ),
      }
      return
    }
    const items = result.data.items ?? []
    operations.value = items.length > 0 ? { kind: 'success', data: items } : { kind: 'empty' }
  }

  async function cancel(environmentIdToCancel: string, revision: number): Promise<EnvironmentOperationMutationResult> {
    cancelDiagnostic.value = null
    cancelling.value = true
    try {
      const result = await cancelEnvironmentOperation({
        path: { environmentId: environmentIdToCancel },
        headers: {
          'Idempotency-Key': idempotencyKey(),
          'If-Match': ifMatch(revision),
        },
      })
      if (result.error) {
        const problem = extractProblemDetails(result.error)
        const diagnostic = makeDiagnostic(
          problem?.diagnosticCode ?? 'ENVIRONMENT_OPERATION_CANCEL_FAILED',
          problem?.detail ?? '取消环境操作失败',
          problem?.retryable ?? true,
        )
        cancelDiagnostic.value = diagnostic
        return { ok: false, diagnostic }
      }
      return { ok: true, accepted: result.data }
    } finally {
      cancelling.value = false
    }
  }

  watch(
    environmentId,
    () => {
      cancelDiagnostic.value = null
      void load()
    },
    { immediate: true },
  )

  return reactive({ operations, load, cancel, cancelling, cancelDiagnostic })
}
