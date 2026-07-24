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
    if (result.error || !result.data) {
      const problem = extractProblemDetails(result.error)
      operations.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? (result.error ? 'OPERATION_LIST_FAILED' : 'OPERATION_LIST_RESPONSE_INVALID'),
          problem?.detail ?? (result.error ? '加载环境操作历史失败' : '环境操作历史响应缺少数据。'),
          problem?.retryable ?? Boolean(result.error),
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
