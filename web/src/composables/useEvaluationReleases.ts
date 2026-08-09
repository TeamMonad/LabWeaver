import { computed, reactive, ref, watch, type Ref } from 'vue'
import {
  createEvaluationRelease,
  getEvaluationRelease,
  listEvaluationReleases,
  withdrawEvaluationRelease,
} from '@/generated/contracts'
import type {
  CandidateApprovalSchema,
  EvaluationCandidateViewSchema,
  EvaluationReleaseSchema,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

const WITHDRAW_REASON = 'LW_EVALUATION_RELEASE_WITHDRAWN_BY_TEACHER'

function errorState(error: unknown, fallbackCode: string, fallbackDetail: string): AsyncState<never> {
  const problem = extractProblemDetails(error)
  const diagnostic = makeDiagnostic(
    problem?.diagnosticCode ?? fallbackCode,
    problem?.detail ?? fallbackDetail,
    problem?.retryable ?? true,
  )
  if (problem?.status === 403) return { kind: 'unauthorized', diagnostic }
  if (problem?.status === 409 || problem?.status === 412) return { kind: 'conflict', diagnostic }
  return { kind: 'error', diagnostic }
}

export function useEvaluationReleases(
  courseId: Ref<string | undefined>,
  candidateView: Ref<EvaluationCandidateViewSchema | null>,
  approval: Ref<CandidateApprovalSchema | null>,
) {
  const releases = ref<AsyncState<EvaluationReleaseSchema[]>>({ kind: 'idle' })
  const selected = ref<AsyncState<EvaluationReleaseSchema>>({ kind: 'idle' })
  const publication = ref<AsyncState<EvaluationReleaseSchema>>({ kind: 'idle' })
  const withdrawingId = ref<string | null>(null)
  const publicationFence = ref<{ fingerprint: string; key: string } | null>(null)
  const withdrawalFences = new Map<string, string>()

  const canPublish = computed(() => (
    !!courseId.value
    && !!candidateView.value
    && approval.value?.decision === 'approved'
    && publication.value.kind !== 'loading'
    && !currentCandidateRelease.value
  ))

  const currentCandidateRelease = computed(() => {
    if (releases.value.kind !== 'success' || !candidateView.value) return null
    return releases.value.data.find((release) => (
      release.candidateId === candidateView.value?.candidate.id
      && release.candidateRevision === candidateView.value?.candidate.revision
      && release.approvalId === approval.value?.id
    )) ?? null
  })

  async function load() {
    const course = courseId.value
    if (!course) {
      releases.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic('COURSE_CONTEXT_REQUIRED', '缺少课程上下文，无法读取 EvaluationRelease。'),
      }
      return
    }
    releases.value = { kind: 'loading', message: '加载 EvaluationRelease…' }
    const response = await listEvaluationReleases({ path: { courseId: course }, query: { limit: 50 } })
    if (response.error) {
      releases.value = errorState(response.error, 'EVALUATION_RELEASE_LIST_FAILED', '加载 EvaluationRelease 失败')
      return
    }
    releases.value = response.data.items.length
      ? { kind: 'success', data: response.data.items }
      : { kind: 'empty' }
  }

  async function publish() {
    const course = courseId.value
    const view = candidateView.value
    const approved = approval.value
    if (!course || !view || approved?.decision !== 'approved') return false
    const body = {
      approvalId: approved.id,
      candidateId: view.candidate.id,
      candidateRevision: view.candidate.revision,
      evaluationSpecSha256: view.candidate.specSha256,
    }
    const fingerprint = JSON.stringify(body)
    if (!publicationFence.value || publicationFence.value.fingerprint !== fingerprint) {
      publicationFence.value = { fingerprint, key: idempotencyKey() }
    }
    publication.value = { kind: 'loading', message: '发布 EvaluationRelease…' }
    const response = await createEvaluationRelease({
      path: { courseId: course },
      headers: { 'Idempotency-Key': publicationFence.value.key },
      body,
    })
    if (response.error) {
      publication.value = errorState(response.error, 'EVALUATION_RELEASE_PUBLISH_FAILED', '发布 EvaluationRelease 失败')
      return false
    }
    publication.value = { kind: 'success', data: response.data }
    selected.value = { kind: 'success', data: response.data }
    await load()
    return true
  }

  async function select(releaseId: string) {
    const course = courseId.value
    if (!course) return
    selected.value = { kind: 'loading', message: '加载 Release 详情…' }
    const response = await getEvaluationRelease({ path: { courseId: course, releaseId } })
    selected.value = response.error
      ? errorState(response.error, 'EVALUATION_RELEASE_LOAD_FAILED', '加载 Release 详情失败')
      : { kind: 'success', data: response.data }
  }

  async function withdraw(release: EvaluationReleaseSchema) {
    const course = courseId.value
    if (!course || release.state !== 'active' || withdrawingId.value) return false
    withdrawingId.value = release.id
    const fenceKey = `${release.id}:${release.revision}`
    const key = withdrawalFences.get(fenceKey) ?? idempotencyKey()
    withdrawalFences.set(fenceKey, key)
    try {
      const response = await withdrawEvaluationRelease({
        path: { courseId: course, releaseId: release.id },
        headers: { 'Idempotency-Key': key, 'If-Match': ifMatch(release.revision) },
        body: { expectedRevision: release.revision, reasonCode: WITHDRAW_REASON },
      })
      if (response.error) {
        selected.value = errorState(response.error, 'EVALUATION_RELEASE_WITHDRAW_FAILED', '撤回 EvaluationRelease 失败')
        return false
      }
      selected.value = { kind: 'success', data: response.data }
      await load()
      return true
    } finally {
      withdrawingId.value = null
    }
  }

  watch(courseId, load, { immediate: true })
  watch(
    () => `${candidateView.value?.candidate.id ?? ''}:${candidateView.value?.candidate.revision ?? ''}:${approval.value?.id ?? ''}`,
    () => {
      publicationFence.value = null
      publication.value = { kind: 'idle' }
    },
  )

  return reactive({
    releases,
    selected,
    publication,
    withdrawingId,
    currentCandidateRelease,
    canPublish,
    load,
    publish,
    select,
    withdraw,
  })
}
