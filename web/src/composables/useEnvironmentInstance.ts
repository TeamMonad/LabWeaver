import { reactive, ref, watch, type Ref } from 'vue'
import type { AxiosError } from 'axios'
import { getEnvironment } from '@/generated/contracts'
import type { EnvironmentInstanceSchema, ProblemDetails } from '@/generated/contracts'
import { makeDiagnostic, type AsyncState } from '@/types/async'

export function useEnvironmentInstance(environmentId: Ref<string | undefined>) {
  const instance = ref<AsyncState<EnvironmentInstanceSchema>>({ kind: 'idle' })
  const polling = ref(false)
  let timer: ReturnType<typeof setTimeout> | null = null

  async function load() {
    const id = environmentId.value
    if (!id) {
      instance.value = { kind: 'idle' }
      return
    }
    if (instance.value.kind !== 'success') {
      instance.value = { kind: 'loading', message: '加载环境状态…' }
    }
    const result = await getEnvironment({ path: { environmentId: id } })
    if (result.error) {
      const problem = ((result.error as AxiosError).response?.data ?? (result.error as { error?: ProblemDetails }).error) as
        | ProblemDetails
        | undefined
      instance.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'ENVIRONMENT_LOAD_FAILED',
          problem?.detail ?? '加载环境状态失败',
          problem?.retryable ?? true,
        ),
      }
      polling.value = false
      return
    }
    instance.value = { kind: 'success', data: result.data }
  }

  async function poll() {
    const id = environmentId.value
    if (!id || !polling.value) return
    await load()
    if (polling.value) {
      timer = setTimeout(poll, 3000)
    }
  }

  function startPolling() {
    polling.value = true
    if (timer) clearTimeout(timer)
    poll()
  }

  function stopPolling() {
    polling.value = false
    if (timer) {
      clearTimeout(timer)
      timer = null
    }
  }

  watch(
    environmentId,
    (id) => {
      if (id) {
        startPolling()
      } else {
        stopPolling()
        instance.value = { kind: 'idle' }
      }
    },
    { immediate: true },
  )

  return reactive({ instance, polling, load, startPolling, stopPolling })
}
