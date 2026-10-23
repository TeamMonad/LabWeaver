<template>
  <section
    v-if="latestSubmission"
    class="submission-tracking"
    aria-labelledby="submission-tracking-heading"
    data-testid="submission-tracking"
  >
    <div class="submission-tracking__head">
      <h5 id="submission-tracking-heading">
        最近一次提交
      </h5>
      <RouterLink
        class="submission-tracking__link"
        :to="latestLink"
      >
        {{ latestRunId ? '查看评测详情' : '在评测结果中查看' }}
      </RouterLink>
    </div>
    <p
      class="submission-tracking__summary"
      role="status"
    >
      {{ latestSummary }}
    </p>
    <SubmissionProgress :stages="latestStages" />
    <SubmissionTaskNotes :record="latestTask" />
  </section>
</template>

<script setup lang="ts">
import { computed, watch } from 'vue'
import { RouterLink } from 'vue-router'
import SubmissionProgress from '@/components/evaluation/SubmissionProgress.vue'
import SubmissionTaskNotes from '@/components/evaluation/SubmissionTaskNotes.vue'
import { useSubmissionTasks } from '@/composables/useEvaluationTasks'
import { useEvaluationResult } from '@/composables/useEvaluationResults'
import { readTrackedSubmissions, useSubmissionDirectory } from '@/composables/useSubmissionDirectory'
import { useVisiblePolling } from '@/composables/useVisiblePolling'
import { evaluationOutcome } from '@/utils/evaluationOutcome'
import { isTerminalRun, readEvaluationTask, submissionStages } from '@/utils/evaluationProgress'
import { formatTimestamp } from '@/utils/format'

/**
 * 环境内“最近一次提交”的进度卡。只记住提交 ID；每个阶段都按 submission
 * 从 getOwnProjectEvaluationTaskForSubmission 读取，终态后再读成绩。
 */
const props = defineProps<{
  projectId: string
  environmentId: string
  /** 刚冻结的提交 ID；变化时重新读取本地记住的提交。 */
  freshSubmissionId?: string | null
}>()

const latestSubmission = computed(() => {
  void props.freshSubmissionId
  return readTrackedSubmissions(props.projectId).find((item) => item.environmentId === props.environmentId) ?? null
})
const trackingProjectId = computed(() => (latestSubmission.value ? props.projectId : undefined))
const submissionDirectory = useSubmissionDirectory(trackingProjectId)
const submissionTasks = useSubmissionTasks(trackingProjectId)
const latestTask = computed(() => (latestSubmission.value ? submissionTasks.records[latestSubmission.value.submissionId] ?? null : null))
const latestProgress = computed(() => (latestTask.value?.fact.kind === 'progress' ? latestTask.value.fact.progress : null))
const latestRunId = computed(() => latestProgress.value?.runId)
const latestEnded = computed(() => Boolean(latestProgress.value && isTerminalRun(latestProgress.value)))
const latestResultState = useEvaluationResult(trackingProjectId, computed(() => (latestEnded.value ? latestRunId.value : undefined)))
const latestResult = computed(() => (latestEnded.value && latestResultState.result.kind === 'success' ? latestResultState.result.data : null))
const latestSummary = computed(() => {
  if (latestResult.value) return evaluationOutcome(latestResult.value).summary
  const fact = latestTask.value?.fact
  if (fact?.kind === 'progress') return `${readEvaluationTask(fact.progress).detail}离开后回到这里或“评测结果”页也能看到这次提交。`
  if (fact?.kind === 'not-created') return '提交已保存，平台还没有为它创建评测。页面会自动更新。'
  return '提交已保存，正在读取评测进度。页面会自动更新，离开后回到这里或“评测结果”页也能看到这次提交。'
})
const latestStages = computed(() => {
  const submission = latestSubmission.value
  if (!submission) return []
  const record = submissionDirectory.records[submission.submissionId]
  return submissionStages({
    frozenAt: record?.kind === 'ready' ? record.frozenAt : submission.submittedAt,
    task: latestTask.value?.fact,
    result: latestResult.value,
  }, formatTimestamp)
})
const latestLink = computed(() => {
  const submissionId = latestSubmission.value?.submissionId
  const query = { projectId: props.projectId, ...(submissionId ? { submissionId } : {}) }
  return latestRunId.value ? { path: `/student/results/${latestRunId.value}`, query } : { path: '/student/results', query }
})

watch([() => latestSubmission.value?.submissionId, trackingProjectId], ([submissionId, project]) => {
  if (!submissionId || !project) return
  void submissionDirectory.resolve([submissionId])
  void submissionTasks.refresh([submissionId])
}, { immediate: true })
useVisiblePolling(
  () => (latestSubmission.value ? submissionTasks.refresh([latestSubmission.value.submissionId]) : undefined),
  computed(() => Boolean(latestSubmission.value) && !latestEnded.value),
)
</script>

<style scoped>
.submission-tracking { display: grid; gap: var(--space-3); margin-top: var(--space-4); padding: var(--space-4); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); background: var(--md-sys-color-surface-container-low); }
.submission-tracking__head { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--space-2) var(--space-4); }
.submission-tracking__head h5 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.submission-tracking__summary { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.submission-tracking__link { color: var(--md-sys-color-primary); font: var(--md-sys-label-large); }
</style>
