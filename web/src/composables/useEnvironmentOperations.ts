import { reactive, ref, watch, type Ref } from 'vue'
import { listEnvironmentOperations } from '@/generated/contracts'
import type { EnvironmentOperationSnapshotSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'

export function useEnvironmentOperations(environmentId: Ref<string | undefined>) {
  const operations = ref<AsyncState<EnvironmentOperationSnapshotSchema[]>>({ kind: 'idle' })

  async function load() {
    const id = environmentId.value
    if (!id) {
      operations.value = { kind: 'idle' }
      return
    }
    operations.value = { kind: 'loading', message: '加载环境操作历史…' }
    const result = await listEnvironmentOperations({ path: { environmentId: id } })
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

  watch(environmentId, load, { immediate: true })

  return reactive({ operations, load })
}
