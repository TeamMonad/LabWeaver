import { reactive, ref, watch, type Ref } from 'vue'
import { getActiveProjectLlmPolicy } from '@/generated/contracts'
import type { ProjectLlmEgressPolicySchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'

export function useActiveProjectLlmPolicy(projectId: Ref<string | null>) {
  const state = ref<AsyncState<ProjectLlmEgressPolicySchema>>({ kind: 'idle' })

  async function load() {
    const id = projectId.value
    if (!id) {
      state.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic('PROJECT_CONTEXT_MISSING', '请选择项目后再加载 LLM 出站策略。', false),
      }
      return
    }

    state.value = { kind: 'loading', message: '加载项目 LLM 策略…' }
    const result = await getActiveProjectLlmPolicy({ path: { projectId: id } })
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      state.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'LLM_POLICY_LOAD_FAILED',
          problem?.detail ?? '加载项目 LLM 策略失败',
          problem?.retryable ?? true,
        ),
      }
      return
    }
    state.value = { kind: 'success', data: result.data }
  }

  watch(projectId, () => void load(), { immediate: true })

  return reactive({ state, load })
}
