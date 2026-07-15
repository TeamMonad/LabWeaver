import { reactive, ref, watch, type Ref } from 'vue'
import type { AxiosError } from 'axios'
import { apiClient } from '@/api/client'
import { getActiveCourseLlmPolicy } from '@/generated/contracts'
import type { CourseLlmEgressPolicySchema, ProblemDetails } from '@/generated/contracts'
import type { AsyncState } from '@/types/async'
import { makeDiagnostic } from '@/types/async'

export function useActiveCourseLlmPolicy(courseId: Ref<string | undefined>) {
  const state = ref<AsyncState<CourseLlmEgressPolicySchema>>({ kind: 'idle' })

  async function load() {
    const id = courseId.value
    if (!id) {
      state.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic(
          'COURSE_CONTEXT_MISSING',
          '课程上下文未绑定，无法加载 LLM 出站策略。请联系架构 Owner 完成 #47 课程成员 API。',
          false,
        ),
      }
      return
    }

    state.value = { kind: 'loading', message: '加载课程 LLM 策略…' }
    const result = await getActiveCourseLlmPolicy({
      client: apiClient,
      path: { courseId: id },
    })

    if (result.error) {
      const problem = ((result.error as AxiosError).response?.data ?? (result.error as { error?: ProblemDetails }).error) as
        | ProblemDetails
        | undefined
      state.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'LLM_POLICY_LOAD_FAILED',
          problem?.detail ?? '加载课程 LLM 策略失败',
          problem?.retryable ?? true,
        ),
      }
      return
    }

    state.value = { kind: 'success', data: result.data }
  }

  watch(courseId, load, { immediate: true })

  return reactive({ state, load })
}
