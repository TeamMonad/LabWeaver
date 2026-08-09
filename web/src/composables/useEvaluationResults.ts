import { reactive, ref, watch, type Ref } from 'vue'
import { getOwnEvaluationResult, listOwnEvaluationResults } from '@/generated/contracts'
import type { StudentEvaluationResultSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'

function resultError(error: unknown, fallbackCode: string, fallbackDetail: string): AsyncState<never> {
  const problem = extractProblemDetails(error)
  const diagnostic = makeDiagnostic(
    problem?.diagnosticCode ?? fallbackCode,
    problem?.detail ?? fallbackDetail,
    problem?.retryable ?? true,
  )
  if (problem?.status === 401 || problem?.status === 403) return { kind: 'unauthorized', diagnostic }
  return { kind: 'error', diagnostic }
}

export function useEvaluationResults(courseId: Ref<string | undefined>) {
  const results = ref<AsyncState<StudentEvaluationResultSchema[]>>({ kind: 'idle' })
  const nextCursor = ref<string | null>(null)
  const loadingMore = ref(false)

  async function load(cursor?: string) {
    const course = courseId.value
    if (!course) {
      results.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic('COURSE_CONTEXT_REQUIRED', '缺少课程上下文，无法读取评测结果。'),
      }
      return
    }
    if (!cursor) results.value = { kind: 'loading', message: '加载评测结果…' }
    else loadingMore.value = true
    try {
      const response = await listOwnEvaluationResults({
        path: { courseId: course },
        query: { cursor, limit: 50 },
      })
      if (response.error) {
        results.value = resultError(response.error, 'EVALUATION_RESULTS_LOAD_FAILED', '加载评测结果失败')
        return
      }
      const previous = cursor && results.value.kind === 'success' ? results.value.data : []
      const items = [...previous, ...response.data.items]
      nextCursor.value = response.data.nextCursor ?? null
      results.value = items.length ? { kind: 'success', data: items } : { kind: 'empty' }
    } finally {
      loadingMore.value = false
    }
  }

  watch(courseId, () => load(), { immediate: true })
  return reactive({ results, nextCursor, loadingMore, load, loadMore: () => nextCursor.value ? load(nextCursor.value) : Promise.resolve() })
}

export function useEvaluationResult(
  courseId: Ref<string | undefined>,
  runId: Ref<string | undefined>,
) {
  const result = ref<AsyncState<StudentEvaluationResultSchema>>({ kind: 'idle' })

  async function load() {
    const course = courseId.value
    const run = runId.value
    if (!course || !run) {
      result.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic('EVALUATION_RESULT_ID_REQUIRED', '缺少课程或 EvaluationRun 标识。'),
      }
      return
    }
    result.value = { kind: 'loading', message: '加载评测详情…' }
    const response = await getOwnEvaluationResult({ path: { courseId: course, runId: run } })
    result.value = response.error
      ? resultError(response.error, 'EVALUATION_RESULT_LOAD_FAILED', '加载评测详情失败')
      : { kind: 'success', data: response.data }
  }

  watch([courseId, runId], load, { immediate: true })
  return reactive({ result, load })
}
