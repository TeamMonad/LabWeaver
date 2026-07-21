import { computed, reactive, ref, watch, type Ref } from 'vue'
import {
  appendEnvironmentCandidateDecision,
  appendEvaluationCandidateDecision,
  createEnvironmentTemplateRelease,
  getAgentRun,
  getEnvironmentCandidate,
  getEvaluationCandidate,
  listEnvironmentTemplateReleases,
} from '@/generated/contracts'
import type {
  AgentRunSchema,
  CandidateApprovalSchema,
  CandidateDecision,
  EnvironmentCandidateViewSchema,
  EvaluationCandidateViewSchema,
  OperationAccepted,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'
import {
  evaluateImageGate,
  type CandidateKind,
} from '@/types/candidate'
import type { DiffChange } from '@/components/common/StructuredDiff.vue'
import { idempotencyKey, ifMatch } from '@/utils/format'

function flattenSpec(value: unknown, prefix = ''): Record<string, string> {
  if (value === null || value === undefined) return {}
  if (typeof value !== 'object') return { [prefix]: String(value) }
  if (Array.isArray(value)) {
    const out: Record<string, string> = {}
    value.forEach((item, index) => {
      const key = prefix ? `${prefix}[${index}]` : `[${index}]`
      Object.assign(out, flattenSpec(item, key))
    })
    return out
  }
  const out: Record<string, string> = {}
  for (const [key, child] of Object.entries(value as Record<string, unknown>)) {
    const next = prefix ? `${prefix}.${key}` : key
    Object.assign(out, flattenSpec(child, next))
  }
  return out
}

export function computeDiff(before: unknown, after: unknown): DiffChange[] {
  const beforeFlat = flattenSpec(before)
  const afterFlat = flattenSpec(after)
  const keys = new Set([...Object.keys(beforeFlat), ...Object.keys(afterFlat)])
  const changes: DiffChange[] = []
  for (const key of Array.from(keys).sort()) {
    const beforeValue = beforeFlat[key]
    const afterValue = afterFlat[key]
    if (beforeValue === undefined) {
      changes.push({ field: key, after: afterValue, kind: 'added' })
    } else if (afterValue === undefined) {
      changes.push({ field: key, before: beforeValue, kind: 'removed' })
    } else if (beforeValue !== afterValue) {
      changes.push({ field: key, before: beforeValue, after: afterValue, kind: 'modified' })
    } else {
      changes.push({ field: key, before: beforeValue, after: afterValue, kind: 'unchanged' })
    }
  }
  return changes
}

function errorDiagnostic(err: unknown, fallbackCode: string, fallbackDetail: string) {
  const problem = extractProblemDetails(err)
  return makeDiagnostic(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackDetail, problem?.retryable ?? true)
}

export function useCandidateApproval(courseId: Ref<string | undefined>, runId: Ref<string | undefined>) {
  const run = ref<AsyncState<AgentRunSchema>>({ kind: 'idle' })
  const environmentCandidate = ref<AsyncState<EnvironmentCandidateViewSchema>>({ kind: 'idle' })
  const evaluationCandidate = ref<AsyncState<EvaluationCandidateViewSchema>>({ kind: 'idle' })
  const previousEnvironmentSpec = ref<AsyncState<unknown>>({ kind: 'idle' })
  const publish = ref<AsyncState<OperationAccepted>>({ kind: 'idle' })
  const deciding = ref<CandidateKind | null>(null)

  async function loadRun() {
    const id = courseId.value
    const rid = runId.value
    if (!id || !rid) {
      run.value = { kind: 'blocked', diagnostic: makeDiagnostic('RUN_ID_MISSING', '缺少 runId，无法加载候选审批。', false) }
      return
    }
    run.value = { kind: 'loading', message: '加载 AgentRun…' }
    const result = await getAgentRun({ path: { courseId: id, runId: rid } })
    if (result.error) {
      run.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'RUN_LOAD_FAILED', '加载 AgentRun 失败') }
      return
    }
    run.value = { kind: 'success', data: result.data }
    await Promise.all([loadEnvironmentCandidate(result.data), loadEvaluationCandidate(result.data), loadPreviousEnvironmentSpec()])
  }

  async function loadEnvironmentCandidate(runData: AgentRunSchema) {
    const id = courseId.value
    const track = runData.tracks.find((t) => t.kind === 'environment')
    const candidateId = track?.candidateId
    if (!id || !candidateId) {
      environmentCandidate.value = { kind: 'blocked', diagnostic: makeDiagnostic('CANDIDATE_ID_MISSING', '该 AgentRun 尚未生成 Environment 候选。', false) }
      return
    }
    environmentCandidate.value = { kind: 'loading', message: '加载 Environment 候选…' }
    const result = await getEnvironmentCandidate({ path: { courseId: id, candidateId } })
    if (result.error) {
      environmentCandidate.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'CANDIDATE_LOAD_FAILED', '加载 Environment 候选失败') }
      return
    }
    environmentCandidate.value = { kind: 'success', data: result.data }
  }

  async function loadEvaluationCandidate(runData: AgentRunSchema) {
    const id = courseId.value
    const track = runData.tracks.find((t) => t.kind === 'evaluation')
    const candidateId = track?.candidateId
    if (!id || !candidateId) {
      evaluationCandidate.value = { kind: 'blocked', diagnostic: makeDiagnostic('CANDIDATE_ID_MISSING', '该 AgentRun 尚未生成 Evaluation 候选。', false) }
      return
    }
    evaluationCandidate.value = { kind: 'loading', message: '加载 Evaluation 候选…' }
    const result = await getEvaluationCandidate({ path: { courseId: id, candidateId } })
    if (result.error) {
      evaluationCandidate.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'CANDIDATE_LOAD_FAILED', '加载 Evaluation 候选失败') }
      return
    }
    evaluationCandidate.value = { kind: 'success', data: result.data }
  }

  async function loadPreviousEnvironmentSpec() {
    const id = courseId.value
    if (!id) return
    const releases = await listEnvironmentTemplateReleases({ path: { courseId: id } })
    if (releases.error || !releases.data.items.length) {
      previousEnvironmentSpec.value = { kind: 'empty' }
      return
    }
    const latest = releases.data.items.reduce((a, b) => (a.version > b.version ? a : b))
    const result = await getEnvironmentCandidate({ path: { courseId: id, candidateId: latest.candidateId } })
    if (result.error) {
      previousEnvironmentSpec.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'PREVIOUS_CANDIDATE_LOAD_FAILED', '加载上一版本候选失败') }
      return
    }
    previousEnvironmentSpec.value = { kind: 'success', data: result.data.candidate.spec }
  }

  const environmentDiff = computed(() => {
    if (environmentCandidate.value.kind !== 'success') return []
    const after = environmentCandidate.value.data.candidate.spec
    const before = previousEnvironmentSpec.value.kind === 'success' ? previousEnvironmentSpec.value.data : null
    return computeDiff(before, after)
  })

  const evaluationDiff = computed(() => {
    if (evaluationCandidate.value.kind !== 'success') return []
    return computeDiff(null, evaluationCandidate.value.data.candidate.spec)
  })

  const latestEnvironmentApproval = computed<CandidateApprovalSchema | null>(() => {
    if (environmentCandidate.value.kind !== 'success') return null
    const approvals = environmentCandidate.value.data.approvals
    return approvals.length > 0 ? approvals[approvals.length - 1] : null
  })

  const latestEvaluationApproval = computed<CandidateApprovalSchema | null>(() => {
    if (evaluationCandidate.value.kind !== 'success') return null
    const approvals = evaluationCandidate.value.data.approvals
    return approvals.length > 0 ? approvals[approvals.length - 1] : null
  })

  const imageGate = computed(() => {
    if (environmentCandidate.value.kind !== 'success') return { status: 'blocked' as const, reasons: ['候选未加载'] }
    const build = environmentCandidate.value.data.build
    return evaluateImageGate(build?.artifact ?? undefined, build?.imagePolicyEvaluation ?? undefined)
  })

  const canPublish = computed(() => {
    const runtimeKind = environmentCandidate.value.kind === 'success'
      ? environmentCandidate.value.data.candidate.spec.runtime.kind
      : undefined
    return (
      environmentCandidate.value.kind === 'success' &&
      latestEnvironmentApproval.value?.decision === 'approved' &&
      (runtimeKind === 'virtual_machine' || imageGate.value.status !== 'blocked') &&
      publish.value.kind !== 'loading'
    )
  })

  async function decide(kind: CandidateKind, decision: CandidateDecision, reason: string) {
    const id = courseId.value
    if (!id) return
    deciding.value = kind
    try {
      if (kind === 'environment') {
        if (environmentCandidate.value.kind !== 'success') return
        const view = environmentCandidate.value.data
        const candidate = view.candidate
        const result = await appendEnvironmentCandidateDecision({
          path: { courseId: id, candidateId: candidate.id },
          headers: { 'If-Match': ifMatch(candidate.revision) },
          body: {
            candidateRevision: candidate.revision,
            candidateSha256: candidate.specSha256,
            decision,
            policyRevision: candidate.policyRevision,
            reason,
            schemaSha256: candidate.schemaSha256,
            trustRevision: view.trustRevision,
          },
        })
        if (result.error) {
          environmentCandidate.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'DECISION_FAILED', '提交审批失败') }
          return
        }
        const approvals = [...view.approvals, result.data]
        environmentCandidate.value = { kind: 'success', data: { ...view, approvals } }
      } else {
        if (evaluationCandidate.value.kind !== 'success') return
        const view = evaluationCandidate.value.data
        const candidate = view.candidate
        const result = await appendEvaluationCandidateDecision({
          path: { courseId: id, candidateId: candidate.id },
          headers: { 'If-Match': ifMatch(candidate.revision) },
          body: {
            candidateRevision: candidate.revision,
            candidateSha256: candidate.specSha256,
            decision,
            policyRevision: candidate.policyRevision,
            reason,
            schemaSha256: candidate.schemaSha256,
            trustRevision: view.trustRevision,
          },
        })
        if (result.error) {
          evaluationCandidate.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'DECISION_FAILED', '提交审批失败') }
          return
        }
        const approvals = [...view.approvals, result.data]
        evaluationCandidate.value = { kind: 'success', data: { ...view, approvals } }
      }
    } finally {
      deciding.value = null
    }
  }

  async function publishRelease() {
    const id = courseId.value
    if (!id) return
    if (environmentCandidate.value.kind !== 'success') return
    const view = environmentCandidate.value.data
    const candidate = view.candidate
    const approval = latestEnvironmentApproval.value
    if (!approval || approval.decision !== 'approved') return
    if (
      candidate.spec.runtime.kind === 'container' &&
      (!view.build?.artifact || !view.build.imagePolicyEvaluation)
    ) return

    publish.value = { kind: 'loading', message: '发布 EnvironmentTemplateRelease…' }
    const result = await createEnvironmentTemplateRelease({
      path: { courseId: id },
      headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(candidate.revision) },
      body: {
        approvalId: approval.id,
        candidateId: candidate.id,
        candidateRevision: candidate.revision,
        environmentSpecSha256: candidate.specSha256,
        runtimeKind: candidate.spec.runtime.kind,
      },
    })
    if (result.error) {
      publish.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'RELEASE_PUBLISH_FAILED', '发布失败') }
      return
    }
    publish.value = { kind: 'success', data: result.data }
  }

  watch([courseId, runId], loadRun, { immediate: true })

  return reactive({
    run,
    environmentCandidate,
    evaluationCandidate,
    previousEnvironmentSpec,
    environmentDiff,
    evaluationDiff,
    latestEnvironmentApproval,
    latestEvaluationApproval,
    imageGate,
    canPublish,
    publish,
    deciding,
    load: loadRun,
    decide,
    publishRelease,
  })
}
