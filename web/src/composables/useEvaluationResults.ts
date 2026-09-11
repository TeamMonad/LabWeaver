import { reactive, ref, watch, type Ref } from 'vue'
import { getOwnProjectEvaluationResult, listOwnProjectEvaluationResults } from '@/generated/contracts'
import type { StudentEvaluationResultSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'

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

export function useEvaluationResults(projectId: Ref<string | undefined>) {
  const results = ref<AsyncState<StudentEvaluationResultSchema[]>>({ kind: 'idle' })
  const nextCursor = ref<string | null>(null)
  const loadingMore = ref(false)
  const loadMoreError = ref<DiagnosticViewModel | null>(null)

  async function load(cursor?: string) {
    const project = projectId.value
    if (!project) {
      results.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic('PROJECT_CONTEXT_REQUIRED', '缺少项目上下文，无法读取评测结果。'),
      }
      return
    }
    if (!cursor) results.value = { kind: 'loading', message: '加载评测结果…' }
    else loadingMore.value = true
    try {
      const response = await listOwnProjectEvaluationResults({
        path: { projectId: project },
        query: { cursor, limit: 50 },
      })
      if (response.error) {
        const problem = extractProblemDetails(response.error)
        const diagnostic = makeDiagnostic(
          problem?.diagnosticCode ?? 'EVALUATION_RESULTS_LOAD_FAILED',
          problem?.detail ?? '加载评测结果失败',
          problem?.retryable ?? true,
        )
        if (cursor && results.value.kind === 'success') {
          // Keep already-loaded rows visible; surface the failure as a
          // recoverable gap so the student can retry just this page.
          loadMoreError.value = diagnostic
        } else {
          results.value = { kind: 'error', diagnostic }
        }
        return
      }
      loadMoreError.value = null
      const previous = cursor && results.value.kind === 'success' ? results.value.data : []
      const items = [...previous, ...response.data.items]
      nextCursor.value = response.data.nextCursor ?? null
      results.value = items.length ? { kind: 'success', data: items } : { kind: 'empty' }
    } finally {
      loadingMore.value = false
    }
  }

  watch(projectId, () => load(), { immediate: true })
  return reactive({
    results,
    nextCursor,
    loadingMore,
    loadMoreError,
    load,
    loadMore: () => (nextCursor.value ? load(nextCursor.value) : Promise.resolve()),
  })
}

export function useEvaluationResult(
  projectId: Ref<string | undefined>,
  runId: Ref<string | undefined>,
) {
  const result = ref<AsyncState<StudentEvaluationResultSchema>>({ kind: 'idle' })

  async function load() {
    const project = projectId.value
    const run = runId.value
    if (!project || !run) {
      result.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic('EVALUATION_RESULT_ID_REQUIRED', '缺少项目或 EvaluationRun 标识。'),
      }
      return
    }
    result.value = { kind: 'loading', message: '加载评测详情…' }
    const response = await getOwnProjectEvaluationResult({ path: { projectId: project, runId: run } })
    result.value = response.error
      ? resultError(response.error, 'EVALUATION_RESULT_LOAD_FAILED', '加载评测详情失败')
      : { kind: 'success', data: response.data }
  }

  watch([projectId, runId], load, { immediate: true })
  return reactive({ result, load })
}
