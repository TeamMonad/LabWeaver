import { reactive, ref, watch, type Ref } from 'vue'
import { getProjectRubricModelInputRules, maintainProjectRubricModelInputRule } from '@/generated/contracts'
import type {
  MaintainRubricModelInputRuleRequestSchema,
  ProjectRubricModelInputRulesSchema,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

/** The shared denial copy talks about sending content; rubric maintenance needs its own reason. */
const RUBRIC_DENIED_MESSAGE = '查看评分规则的模型使用设置需要是课程当前的授课教师并能查看材料项目，修改还需要材料项目的编辑权限；评分规则必须属于当前已发布的评测版本。'

/** What a maintenance request ended in; `conflict` means the policy revision moved and the rules were read again. */
export type RubricRuleMaintainResult = 'saved' | 'conflict' | 'denied' | 'failed' | 'skipped'

function rubricDiagnostic(error: unknown, fallbackCode: string, fallbackMessage: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  const diagnostic = makeDiagnostic(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackMessage, problem?.retryable ?? true, problem)
  if (diagnostic.code === 'LW_MODEL_INPUT_AUTHORIZATION_DENIED') diagnostic.message = RUBRIC_DENIED_MESSAGE
  return diagnostic
}

/**
 * Teacher view of the active policy's rubric model-input rules (#216).
 * Visibility and model-use approval are separate server-side decisions; this
 * composable only reads and submits them through the generated SDK.
 */
export function useRubricModelInputRules(projectId: Ref<string | null>) {
  const state = ref<AsyncState<ProjectRubricModelInputRulesSchema>>({ kind: 'idle' })
  const acting = ref(false)
  const outcome = ref<DiagnosticViewModel | null>(null)
  let generation = 0
  let pendingFingerprint: string | null = null
  let pendingKey: string | null = null

  async function load() {
    const id = projectId.value
    const current = ++generation
    if (!id) {
      state.value = { kind: 'idle' }
      return
    }
    state.value = { kind: 'loading', message: '加载评分规则的模型使用设置…' }
    const result = await getProjectRubricModelInputRules({ path: { projectId: id } })
    if (current !== generation) return
    if (result.error) {
      state.value = { kind: 'error', diagnostic: rubricDiagnostic(result.error, 'RUBRIC_RULES_LOAD_FAILED', '加载评分规则的模型使用设置失败') }
      return
    }
    state.value = { kind: 'success', data: result.data }
  }

  async function maintain(request: MaintainRubricModelInputRuleRequestSchema): Promise<RubricRuleMaintainResult> {
    const id = projectId.value
    if (!id || acting.value || state.value.kind !== 'success') return 'skipped'
    const expectedRevision = state.value.data.policyRevision
    // An unchanged retry after a lost response reuses its key so the server replays one result.
    const fingerprint = JSON.stringify({ id, expectedRevision, request })
    if (fingerprint !== pendingFingerprint || !pendingKey) {
      pendingFingerprint = fingerprint
      pendingKey = idempotencyKey()
    }
    const current = generation
    acting.value = true
    outcome.value = null
    try {
      const result = await maintainProjectRubricModelInputRule({
        path: { projectId: id },
        headers: { 'Idempotency-Key': pendingKey, 'If-Match': ifMatch(expectedRevision) },
        body: request,
      })
      if (current !== generation || projectId.value !== id) return 'skipped'
      if (result.error) {
        const diagnostic = rubricDiagnostic(result.error, 'RUBRIC_RULE_UPDATE_FAILED', '评分规则的模型使用设置没有保存')
        outcome.value = diagnostic
        // Another change moved the policy revision; read the current rules before the teacher retries.
        if (diagnostic.status === 409 || diagnostic.status === 412) {
          await load()
          return 'conflict'
        }
        return diagnostic.status === 403 ? 'denied' : 'failed'
      }
      pendingFingerprint = null
      pendingKey = null
      state.value = { kind: 'success', data: result.data }
      outcome.value = makeDiagnostic('RUBRIC_RULE_UPDATE_ACCEPTED', '评分规则的模型使用设置已保存。', false)
      return 'saved'
    } finally {
      acting.value = false
    }
  }

  watch(projectId, () => {
    outcome.value = null
    pendingFingerprint = null
    pendingKey = null
    void load()
  }, { immediate: true })

  return reactive({ state, acting, outcome, load, maintain })
}
