import { reactive, ref, watch, type Ref } from 'vue'
import { getOwnProjectEvaluationResult, listOwnProjectEvaluationResults } from '@/generated/contracts'
import type { StudentEvaluationResultSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'

type ResultErrorState = Extract<AsyncState<never>, { kind: 'error' | 'unauthorized' }>

function resultError(error: unknown, fallbackCode: string, fallbackDetail: string): ResultErrorState {
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
  let loadGeneration = 0
  const inFlightPages = new Map<string, number>()
  const loadedCursors = new Set<string>()

  function pageKey(project: string, cursor?: string): string {
    return `${project}\u0000${cursor ?? ''}`
  }

  function applyError(error: unknown, cursor: string | undefined) {
    const state = resultError(error, 'EVALUATION_RESULTS_LOAD_FAILED', '加载评测结果失败')
    if (cursor && results.value.kind === 'success') {
      // Keep already-loaded rows visible; surface the failure as a
      // recoverable gap so the student can retry just this page.
      loadMoreError.value = state.diagnostic
    } else {
      results.value = state
    }
  }

  async function load(cursor?: string) {
    const project = projectId.value
    if (!project) {
      ++loadGeneration
      nextCursor.value = null
      loadingMore.value = false
      loadMoreError.value = null
      results.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic('PROJECT_CONTEXT_REQUIRED', '缺少项目上下文，无法读取评测结果。'),
      }
      return
    }

    const key = pageKey(project, cursor)
    if (inFlightPages.has(key) || (cursor && loadedCursors.has(cursor))) return
    const generation = ++loadGeneration
    inFlightPages.set(key, generation)
    if (!cursor) {
      loadedCursors.clear()
      nextCursor.value = null
      loadMoreError.value = null
      results.value = { kind: 'loading', message: '加载评测结果…' }
    } else {
      loadingMore.value = true
    }
    try {
      let response
      try {
        response = await listOwnProjectEvaluationResults({
          path: { projectId: project },
          query: { cursor, limit: 50 },
        })
      } catch (error) {
        if (generation !== loadGeneration || projectId.value !== project) return
        applyError(error, cursor)
        return
      }
      if (generation !== loadGeneration || projectId.value !== project) return
      if (response.error) {
        applyError(response.error, cursor)
        return
      }
      if (response.data.items.some((item) => item.projectId !== project)) {
        results.value = {
          kind: 'error',
          diagnostic: makeDiagnostic(
            'EVALUATION_RESULTS_STALE_CONTEXT',
            '评测结果返回了其他项目的数据，已停止显示。',
            false,
          ),
        }
        return
      }
      loadedCursors.add(cursor ?? '')
      loadMoreError.value = null
      const previous = cursor && results.value.kind === 'success' ? results.value.data : []
      const items = [...previous, ...response.data.items]
      nextCursor.value = response.data.nextCursor ?? null
      results.value = items.length ? { kind: 'success', data: items } : { kind: 'empty' }
    } finally {
      if (inFlightPages.get(key) === generation) inFlightPages.delete(key)
      if (generation === loadGeneration && projectId.value === project) loadingMore.value = false
    }
  }

  watch(
    projectId,
    () => {
      nextCursor.value = null
      loadingMore.value = false
      loadMoreError.value = null
      inFlightPages.clear()
      loadedCursors.clear()
      void load()
    },
    { immediate: true },
  )
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
  let loadGeneration = 0

  async function load() {
    const project = projectId.value
    const run = runId.value
    const generation = ++loadGeneration
    if (!project || !run) {
      result.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic('EVALUATION_RESULT_ID_REQUIRED', '缺少项目或 EvaluationRun 标识。'),
      }
      return
    }
    result.value = { kind: 'loading', message: '加载评测详情…' }
    try {
      const response = await getOwnProjectEvaluationResult({ path: { projectId: project, runId: run } })
      if (generation !== loadGeneration || projectId.value !== project || runId.value !== run) return
      if (response.error) {
        result.value = resultError(response.error, 'EVALUATION_RESULT_LOAD_FAILED', '加载评测详情失败')
      } else if (response.data.projectId !== project || response.data.runId !== run) {
        result.value = {
          kind: 'error',
          diagnostic: makeDiagnostic(
            'EVALUATION_RESULT_STALE_CONTEXT',
            '评测详情返回了其他项目或运行的数据，已停止显示。',
            false,
          ),
        }
      } else {
        result.value = { kind: 'success', data: response.data }
      }
    } catch (error) {
      if (generation !== loadGeneration || projectId.value !== project || runId.value !== run) return
      result.value = resultError(error, 'EVALUATION_RESULT_LOAD_FAILED', '加载评测详情失败')
    }
  }

  watch([projectId, runId], load, { immediate: true })
  return reactive({ result, load })
}
