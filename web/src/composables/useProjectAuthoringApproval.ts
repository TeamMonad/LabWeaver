import { computed, onScopeDispose, reactive, ref, watch, type Ref } from 'vue'
import {
  completeProjectAuthoringApproval,
  getProjectAgentRun,
  getProjectAuthoringApproval,
  getProjectEnvironmentCandidate,
  getProjectEvaluationCandidate,
  getProjectProblemPackage,
} from '@/generated/contracts'
import type {
  AgentRunSchema,
  AuthoringApprovalSchema,
  AuthoringApprovalPublicationStatusSchema,
  CompleteAuthoringApprovalRequestSchemaImageArtifact,
  EnvironmentCandidateViewSchema,
  EvaluationCandidateViewSchema,
  ProblemPackageSchema,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey } from '@/utils/format'

function errorDiagnostic(error: unknown, fallbackCode: string, fallbackMessage: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackMessage, problem?.retryable ?? true)
}

/**
 * Loads the complete, project-scoped authoring review and submits the one
 * teacher command that binds both candidates, the package revision and the
 * resolved image artifact. Candidate decisions remain read-only context here;
 * the durable approval is the single authoritative publish prerequisite.
 */
export function useProjectAuthoringApproval(
  projectId: Ref<string | null>,
  runId: Ref<string | undefined>,
  approvalId: Ref<string | undefined> = ref(undefined),
) {
  const run = ref<AsyncState<AgentRunSchema>>({ kind: 'idle' })
  const environmentCandidate = ref<AsyncState<EnvironmentCandidateViewSchema>>({ kind: 'idle' })
  const evaluationCandidate = ref<AsyncState<EvaluationCandidateViewSchema>>({ kind: 'idle' })
  const problemPackage = ref<AsyncState<ProblemPackageSchema>>({ kind: 'idle' })
  const approval = ref<AsyncState<AuthoringApprovalSchema>>({ kind: 'idle' })
  const publication = ref<AsyncState<AuthoringApprovalPublicationStatusSchema>>({ kind: 'idle' })
  const acting = ref(false)
  let loadGeneration = 0
  let publicationGeneration = 0
  let publicationPollTimer: ReturnType<typeof setTimeout> | null = null
  const CANDIDATE_POLL_INTERVAL_MS = 3000
  const CANDIDATE_NOT_FOUND_MAX_RETRIES = 100
  let environmentCandidatePollTimer: ReturnType<typeof setTimeout> | null = null
  let evaluationCandidatePollTimer: ReturnType<typeof setTimeout> | null = null
  let environmentCandidateRetryId: string | null = null
  let evaluationCandidateRetryId: string | null = null
  let environmentCandidateRetryCount = 0
  let evaluationCandidateRetryCount = 0
  let completionIdempotencyKey: string | null = null
  let completionFingerprint: string | null = null

  function stopPublicationPolling() {
    if (publicationPollTimer) {
      clearTimeout(publicationPollTimer)
      publicationPollTimer = null
    }
  }

  function stopCandidatePolling() {
    if (environmentCandidatePollTimer) {
      clearTimeout(environmentCandidatePollTimer)
      environmentCandidatePollTimer = null
    }
    if (evaluationCandidatePollTimer) {
      clearTimeout(evaluationCandidatePollTimer)
      evaluationCandidatePollTimer = null
    }
  }

  function resetCandidatePolling() {
    stopCandidatePolling()
    environmentCandidateRetryId = null
    evaluationCandidateRetryId = null
    environmentCandidateRetryCount = 0
    evaluationCandidateRetryCount = 0
  }

  function scheduleEnvironmentCandidateRetry(project: string, candidateId: string, generation: number) {
    if (environmentCandidatePollTimer) clearTimeout(environmentCandidatePollTimer)
    environmentCandidatePollTimer = setTimeout(() => {
      environmentCandidatePollTimer = null
      if (generation !== loadGeneration) return
      void loadEnvironmentCandidate(project, candidateId, generation, true)
    }, CANDIDATE_POLL_INTERVAL_MS)
  }

  function scheduleEvaluationCandidateRetry(project: string, candidateId: string, generation: number) {
    if (evaluationCandidatePollTimer) clearTimeout(evaluationCandidatePollTimer)
    evaluationCandidatePollTimer = setTimeout(() => {
      evaluationCandidatePollTimer = null
      if (generation !== loadGeneration) return
      void loadEvaluationCandidate(project, candidateId, generation, true)
    }, CANDIDATE_POLL_INTERVAL_MS)
  }

  async function loadEnvironmentCandidate(project: string, candidateId: string, generation: number, silent = false) {
    if (generation !== loadGeneration) return
    if (!silent || environmentCandidateRetryId !== candidateId) {
      environmentCandidateRetryId = candidateId
      environmentCandidateRetryCount = 0
    }
    if (!silent) environmentCandidate.value = { kind: 'loading', message: '加载 Environment 候选…' }
    const result = await getProjectEnvironmentCandidate({ path: { projectId: project, candidateId } })
    if (generation !== loadGeneration) return
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      const candidateProjectionPending = result.response?.status === 404 && problem?.diagnosticCode === 'LW_CANDIDATE_NOT_FOUND'
      if (candidateProjectionPending && environmentCandidateRetryCount < CANDIDATE_NOT_FOUND_MAX_RETRIES) {
        environmentCandidateRetryCount += 1
        environmentCandidate.value = { kind: 'loading', message: '等待 Environment 候选同步…' }
        scheduleEnvironmentCandidateRetry(project, candidateId, generation)
        return
      }
      environmentCandidate.value = candidateProjectionPending
        ? { kind: 'error', diagnostic: makeDiagnostic('LW_CANDIDATE_NOT_FOUND', 'Environment 候选在限定时间内仍未同步，请重试。', true) }
        : { kind: 'error', diagnostic: errorDiagnostic(result.error, 'PROJECT_APPROVAL_ENVIRONMENT_LOAD_FAILED', '加载 Environment 候选失败') }
      return
    }
    environmentCandidateRetryCount = 0
    environmentCandidate.value = { kind: 'success', data: result.data }
  }

  async function loadEvaluationCandidate(project: string, candidateId: string, generation: number, silent = false) {
    if (generation !== loadGeneration) return
    if (!silent || evaluationCandidateRetryId !== candidateId) {
      evaluationCandidateRetryId = candidateId
      evaluationCandidateRetryCount = 0
    }
    if (!silent) evaluationCandidate.value = { kind: 'loading', message: '加载 Evaluation 候选…' }
    const result = await getProjectEvaluationCandidate({ path: { projectId: project, candidateId } })
    if (generation !== loadGeneration) return
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      const candidateProjectionPending = result.response?.status === 404 && problem?.diagnosticCode === 'LW_CANDIDATE_NOT_FOUND'
      if (candidateProjectionPending && evaluationCandidateRetryCount < CANDIDATE_NOT_FOUND_MAX_RETRIES) {
        evaluationCandidateRetryCount += 1
        evaluationCandidate.value = { kind: 'loading', message: '等待 Evaluation 候选同步…' }
        scheduleEvaluationCandidateRetry(project, candidateId, generation)
        return
      }
      evaluationCandidate.value = candidateProjectionPending
        ? { kind: 'error', diagnostic: makeDiagnostic('LW_CANDIDATE_NOT_FOUND', 'Evaluation 候选在限定时间内仍未同步，请重试。', true) }
        : { kind: 'error', diagnostic: errorDiagnostic(result.error, 'PROJECT_APPROVAL_EVALUATION_LOAD_FAILED', '加载 Evaluation 候选失败') }
      return
    }
    evaluationCandidateRetryCount = 0
    evaluationCandidate.value = { kind: 'success', data: result.data }
  }

  function schedulePublicationPoll(data: AuthoringApprovalPublicationStatusSchema) {
    stopPublicationPolling()
    if (data.status === 'ready' || data.status === 'failed') return
    publicationPollTimer = setTimeout(() => void loadPublication(data.approval.id, true), 3000)
  }

  async function loadPublication(id = approvalId.value, silent = false) {
    const project = projectId.value
    const publicationId = id?.trim()
    const generation = ++publicationGeneration
    stopPublicationPolling()
    if (!project || !publicationId) {
      publication.value = { kind: 'idle' }
      return
    }
    if (!silent) publication.value = { kind: 'loading', message: '加载发布状态…' }
    const result = await getProjectAuthoringApproval({
      path: { projectId: project, approvalId: publicationId },
    })
    if (generation !== publicationGeneration) return
    if (result.error) {
      publication.value = {
        kind: 'error',
        diagnostic: errorDiagnostic(result.error, 'PROJECT_APPROVAL_PUBLICATION_LOAD_FAILED', '加载发布状态失败'),
      }
      return
    }
    publication.value = { kind: 'success', data: result.data }
    approval.value = { kind: 'success', data: result.data.approval }
    schedulePublicationPoll(result.data)
  }

  const imageArtifact = computed<CompleteAuthoringApprovalRequestSchemaImageArtifact | null>(() => {
    if (environmentCandidate.value.kind !== 'success') return null
    const artifact = environmentCandidate.value.data.imageArtifact
    return artifact ? (artifact as CompleteAuthoringApprovalRequestSchemaImageArtifact) : null
  })

  const canApprove = computed(() => {
    return (
      run.value.kind === 'success' &&
      environmentCandidate.value.kind === 'success' &&
      evaluationCandidate.value.kind === 'success' &&
      problemPackage.value.kind === 'success' &&
      imageArtifact.value !== null &&
      // An approval id identifies an already-created immutable approval. Its
      // publication may still be pending, but the same candidate tuple cannot
      // be approved again. Keep the form closed while that status is rebuilt.
      !approvalId.value &&
      approval.value.kind !== 'success' &&
      approval.value.kind !== 'loading' &&
      !acting.value
    )
  })

  function reset() {
    publicationGeneration += 1
    run.value = { kind: 'idle' }
    environmentCandidate.value = { kind: 'idle' }
    evaluationCandidate.value = { kind: 'idle' }
    problemPackage.value = { kind: 'idle' }
    approval.value = { kind: 'idle' }
    publication.value = { kind: 'idle' }
    completionIdempotencyKey = null
    completionFingerprint = null
    stopPublicationPolling()
    resetCandidatePolling()
  }

  async function load() {
    const generation = ++loadGeneration
    const id = projectId.value
    const rid = runId.value
    reset()
    if (!id || !rid) {
      run.value = { kind: 'blocked', diagnostic: makeDiagnostic('PROJECT_APPROVAL_CONTEXT_MISSING', '请选择项目并提供 AgentRun ID。', false) }
      return
    }

    if (approvalId.value) void loadPublication(approvalId.value)

    run.value = { kind: 'loading', message: '加载 AgentRun…' }
    const runResult = await getProjectAgentRun({ path: { projectId: id, runId: rid } })
    if (generation !== loadGeneration) return
    if (runResult.error) {
      run.value = { kind: 'error', diagnostic: errorDiagnostic(runResult.error, 'PROJECT_APPROVAL_RUN_LOAD_FAILED', '加载 AgentRun 失败') }
      return
    }
    run.value = { kind: 'success', data: runResult.data }

    const environmentTrack = runResult.data.tracks.find((track) => track.kind === 'environment')
    const evaluationTrack = runResult.data.tracks.find((track) => track.kind === 'evaluation')
    const candidateJobs: Promise<unknown>[] = []

    if (environmentTrack?.candidateId) {
      environmentCandidate.value = { kind: 'loading', message: '加载 Environment 候选…' }
      candidateJobs.push(loadEnvironmentCandidate(id, environmentTrack.candidateId, generation))
    } else {
      environmentCandidate.value = { kind: 'blocked', diagnostic: makeDiagnostic('PROJECT_APPROVAL_ENVIRONMENT_MISSING', '该 AgentRun 尚未生成 Environment 候选。', false) }
    }

    if (evaluationTrack?.candidateId) {
      evaluationCandidate.value = { kind: 'loading', message: '加载 Evaluation 候选…' }
      candidateJobs.push(loadEvaluationCandidate(id, evaluationTrack.candidateId, generation))
    } else {
      evaluationCandidate.value = { kind: 'blocked', diagnostic: makeDiagnostic('PROJECT_APPROVAL_EVALUATION_MISSING', '该 AgentRun 尚未生成 Evaluation 候选。', false) }
    }

    problemPackage.value = { kind: 'loading', message: '加载材料包…' }
    candidateJobs.push(
      getProjectProblemPackage({ path: { projectId: id, packageId: runResult.data.packageId } }).then((result) => {
        if (generation !== loadGeneration) return
        problemPackage.value = result.error
          ? { kind: 'error', diagnostic: errorDiagnostic(result.error, 'PROJECT_APPROVAL_PACKAGE_LOAD_FAILED', '加载材料包失败') }
          : { kind: 'success', data: result.data }
      }),
    )

    await Promise.all(candidateJobs)
  }

  async function complete(reason: string): Promise<boolean> {
    const id = projectId.value
    if (!id || !canApprove.value || acting.value) return false
    const runData = run.value.kind === 'success' ? run.value.data : null
    const environment = environmentCandidate.value.kind === 'success' ? environmentCandidate.value.data : null
    const evaluation = evaluationCandidate.value.kind === 'success' ? evaluationCandidate.value.data : null
    const pkg = problemPackage.value.kind === 'success' ? problemPackage.value.data : null
    const artifact = imageArtifact.value
    const trimmedReason = reason.trim()
    if (!runData || !environment || !evaluation || !pkg || !artifact || !trimmedReason || trimmedReason.length > 500) return false

    const body = {
      projectId: id,
      courseId: runData.courseId ?? null,
      packageId: pkg.id,
      packageRevision: pkg.revision,
      environmentCandidateId: environment.candidate.id,
      environmentCandidateRevision: environment.candidate.revision,
      evaluationCandidateId: evaluation.candidate.id,
      evaluationCandidateRevision: evaluation.candidate.revision,
      imageArtifact: artifact,
      reason: trimmedReason,
    }
    const fingerprint = JSON.stringify(body)
    if (completionFingerprint !== fingerprint || !completionIdempotencyKey) {
      completionFingerprint = fingerprint
      completionIdempotencyKey = idempotencyKey()
    }

    acting.value = true
    approval.value = { kind: 'loading', message: '提交完整实验包批准…' }
    try {
      const result = await completeProjectAuthoringApproval({
        path: { projectId: id },
        headers: { 'Idempotency-Key': completionIdempotencyKey },
        body,
      })
      if (result.error) {
        approval.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'PROJECT_AUTHORING_APPROVAL_FAILED', '提交完整实验包批准失败') }
        return false
      }
      approval.value = { kind: 'success', data: result.data }
      return true
    } finally {
      acting.value = false
    }
  }

  watch([projectId, runId, approvalId], () => void load(), { immediate: true })
  onScopeDispose(() => {
    loadGeneration += 1
    publicationGeneration += 1
    resetCandidatePolling()
    stopPublicationPolling()
  })

  return reactive({
    run,
    environmentCandidate,
    evaluationCandidate,
    problemPackage,
    approval,
    publication,
    imageArtifact,
    canApprove,
    acting,
    load,
    loadPublication,
    stopPublicationPolling,
    complete,
  })
}
