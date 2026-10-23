<template>
  <section class="results-page" aria-labelledby="results-heading">
    <header class="results-header">
      <h1 id="results-heading">评测结果</h1>
      <p>每次提交从保存到出分的过程都在这里。有提交在等待结果时，页面会自动更新；页面在后台时暂停更新。</p>
    </header>

    <section
      v-if="pending.length"
      class="results-section"
      aria-labelledby="pending-heading"
      data-testid="pending-submissions"
    >
      <h2 id="pending-heading">进行中的评测</h2>
      <p class="section-hint" role="status">
        {{ pending.length }} 个提交还没有结果。进度来自平台记录，页面会自动更新。这里只列出在这个浏览器提交的评测；换设备后，出分时结果会出现在下方列表。
      </p>
      <ul class="result-list">
        <li
          v-for="item in pending"
          :key="item.submissionId"
          class="result-card md-card"
          :class="{ 'result-card--highlight': item.submissionId === highlightedSubmission }"
          :data-submission-id="item.submissionId"
        >
          <div class="result-card__head">
            <div class="result-card__title">
              <h3>
                <RouterLink
                  v-if="taskRunId(item.submissionId)"
                  class="result-link"
                  :to="{ path: `/student/results/${taskRunId(item.submissionId)}`, query: { projectId, submissionId: item.submissionId } }"
                >
                  {{ submissionTitle(item.submissionId) }}
                </RouterLink>
                <template v-else>{{ submissionTitle(item.submissionId) }}</template>
              </h3>
              <p class="result-card__meta">提交于 {{ formatTimestamp(submittedAt(item)) }}</p>
            </div>
            <span class="outcome-chip outcome-chip--pending">{{ pendingTaskLabel(taskFact(item.submissionId)) }}</span>
          </div>
          <SubmissionProgress :stages="pendingStages(item)" :label="`${submissionTitle(item.submissionId)}的提交进度`" />
          <SubmissionTaskNotes :record="tasks.records[item.submissionId]" />
        </li>
      </ul>
    </section>

    <section class="results-section" aria-labelledby="finished-heading">
      <h2 id="finished-heading">已结束的评测</h2>
      <AsyncStateView
        :state="evaluation.results"
        empty-text="当前项目还没有已结束的评测。提交后，结果会显示在这里。"
        @retry="evaluation.load()"
      >
        <template #success="{ data: results }">
          <ul class="result-list">
            <li
              v-for="result in results"
              :key="result.runId"
              class="result-card md-card"
              :class="[`result-card--${evaluationOutcome(result).kind}`, { 'result-card--highlight': result.frozenSubmissionId === highlightedSubmission }]"
              :data-run-id="result.runId"
            >
              <div class="result-card__head">
                <div class="result-card__title">
                  <h3>
                    <RouterLink
                      class="result-link"
                      :to="{ path: `/student/results/${result.runId}`, query: { projectId: result.projectId } }"
                    >
                      {{ submissionTitle(result.frozenSubmissionId) }}
                    </RouterLink>
                  </h3>
                  <p class="result-card__meta">
                    <template v-if="frozenAt(result.frozenSubmissionId)">提交于 {{ formatTimestamp(frozenAt(result.frozenSubmissionId)!) }} · </template>
                    完成于 {{ formatTimestamp(result.completedAt) }}
                  </p>
                </div>
                <div class="result-card__outcome">
                  <span class="outcome-chip" :class="`outcome-chip--${evaluationOutcome(result).kind}`">
                    {{ evaluationOutcome(result).label }}
                  </span>
                  <strong v-if="evaluationOutcome(result).score" class="result-score">
                    {{ evaluationOutcome(result).score!.awarded }}<span> / {{ evaluationOutcome(result).score!.max }}</span>
                  </strong>
                </div>
              </div>
              <p class="result-card__summary">
                {{ evaluationOutcome(result).summary }}
                <template v-if="evaluationOutcome(result).reason">原因：{{ evaluationOutcome(result).reason }}。</template>
              </p>
            </li>
          </ul>
          <button
            v-if="evaluation.nextCursor"
            type="button"
            class="outlined-button load-more"
            :disabled="evaluation.loadingMore"
            @click="evaluation.loadMore()"
          >
            {{ evaluation.loadingMore ? '加载中…' : '加载更多' }}
          </button>
          <DiagnosticBanner
            v-if="evaluation.loadMoreError"
            class="load-more-error"
            :code="evaluation.loadMoreError.code"
            :request-id="evaluation.loadMoreError.requestId"
            :next-step="evaluation.loadMoreError.nextStep"
            :message="evaluation.loadMoreError.message"
            :retryable="evaluation.loadMoreError.retryable"
            severity="warning"
            @retry="evaluation.loadMore()"
          />
        </template>
      </AsyncStateView>
    </section>
  </section>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SubmissionProgress from '@/components/evaluation/SubmissionProgress.vue'
import SubmissionTaskNotes from '@/components/evaluation/SubmissionTaskNotes.vue'
import { useSubmissionTasks } from '@/composables/useEvaluationTasks'
import { useProjects } from '@/composables/useProjects'
import { useEvaluationResults } from '@/composables/useEvaluationResults'
import {
  forgetSubmission,
  readTrackedSubmissions,
  useSubmissionDirectory,
  type TrackedSubmission,
} from '@/composables/useSubmissionDirectory'
import { useVisiblePolling } from '@/composables/useVisiblePolling'
import { evaluationOutcome } from '@/utils/evaluationOutcome'
import { isTerminalRun, pendingTaskLabel, submissionStages, type SubmissionTaskFact } from '@/utils/evaluationProgress'
import { formatTimestamp } from '@/utils/format'

const route = useRoute()
const projects = useProjects()
const routeProjectId = computed(() => {
  const id = typeof route.query.projectId === 'string' ? route.query.projectId.trim() : ''
  return id || undefined
})
const projectId = computed(() => routeProjectId.value ?? projects.selectedProjectId ?? undefined)
const highlightedSubmission = computed(() => typeof route.query.submissionId === 'string' ? route.query.submissionId : null)
const evaluation = useEvaluationResults(projectId)
const directory = useSubmissionDirectory(projectId)
const tasks = useSubmissionTasks(projectId)

const tracked = ref<TrackedSubmission[]>([])
watch(projectId, (id) => { tracked.value = id ? readTrackedSubmissions(id) : [] }, { immediate: true })

const loadedResults = computed(() => evaluation.results.kind === 'success' ? evaluation.results.data : [])
const resultSubmissionIds = computed(() => new Set(loadedResults.value.map((item) => item.frozenSubmissionId)))

/** Submissions this browser started that have no terminal result yet. */
const pending = computed(() => {
  if (evaluation.results.kind !== 'success' && evaluation.results.kind !== 'empty') return []
  return tracked.value.filter((item) => !resultSubmissionIds.value.has(item.submissionId) && directory.records[item.submissionId]?.kind !== 'gone')
})

// Once a tracked submission has a result, or the server no longer returns it,
// stop tracking it; the result list is the record from then on.
watch([resultSubmissionIds, () => directory.records], () => {
  const project = projectId.value
  if (!project) return
  const settled = tracked.value.filter((item) => resultSubmissionIds.value.has(item.submissionId) || directory.records[item.submissionId]?.kind === 'gone')
  if (!settled.length) return
  for (const item of settled) forgetSubmission(project, item.submissionId)
  tracked.value = readTrackedSubmissions(project)
}, { deep: true })

watch(
  () => [...loadedResults.value.map((item) => item.frozenSubmissionId), ...tracked.value.map((item) => item.submissionId)],
  (ids) => { void directory.resolve(ids) },
  { immediate: true },
)

function taskFact(submissionId: string): SubmissionTaskFact {
  return tasks.records[submissionId]?.fact ?? { kind: 'unread' }
}

function taskRunId(submissionId: string): string | null {
  const fact = taskFact(submissionId)
  return fact.kind === 'progress' ? fact.progress.runId : null
}

/** Waiting submissions whose evaluation has not ended; a finished run needs no more reads. */
const activeSubmissionIds = computed(() => pending.value
  .filter((item) => {
    const fact = taskFact(item.submissionId)
    return fact.kind !== 'progress' || !isTerminalRun(fact.progress)
  })
  .map((item) => item.submissionId))

// First read of each waiting submission; later reads come from polling.
watch(
  () => pending.value.map((item) => item.submissionId).join('\u0000'),
  () => { void tasks.refresh(pending.value.map((item) => item.submissionId).filter((id) => !tasks.records[id])) },
  { immediate: true },
)

// Ended runs need no more progress reads, but the list keeps refreshing until
// their result row arrives and the waiting card is removed.
useVisiblePolling(
  () => Promise.all([evaluation.refresh(), tasks.refresh(activeSubmissionIds.value)]),
  computed(() => pending.value.length > 0),
)

function record(submissionId: string) {
  const value = directory.records[submissionId]
  return value?.kind === 'ready' ? value : null
}

function frozenAt(submissionId: string) {
  return record(submissionId)?.frozenAt ?? null
}

/** Experiment environment name; never a run or submission identifier. */
function submissionTitle(submissionId: string) {
  return record(submissionId)?.environmentLabel ?? '实验提交'
}

function submittedAt(item: TrackedSubmission) {
  return record(item.submissionId)?.frozenAt ?? item.submittedAt
}

function pendingStages(item: TrackedSubmission) {
  return submissionStages({ frozenAt: record(item.submissionId)?.frozenAt ?? null, task: taskFact(item.submissionId) }, formatTimestamp)
}
</script>

<style scoped>
.results-page { display: flex; flex-direction: column; gap: var(--space-6); max-width: var(--content-max-width); margin: 0 auto; }
.results-header h1 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-headline-small); }
.results-header p, .section-hint { margin: var(--space-1) 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.results-section { display: grid; gap: var(--space-3); }
.results-section h2 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.result-list { display: grid; gap: var(--space-3); list-style: none; margin: 0; padding: 0; }
.result-card { display: grid; gap: var(--space-3); padding: var(--space-4) var(--space-5); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.result-card--highlight { border-color: var(--md-sys-color-primary); box-shadow: 0 0 0 1px var(--md-sys-color-primary); }
.result-card--not-completed { box-shadow: inset 3px 0 0 var(--md-sys-color-warning, var(--md-sys-color-tertiary)); }
.result-card__head { display: flex; align-items: flex-start; justify-content: space-between; gap: var(--space-4); }
.result-card__title { display: grid; gap: var(--space-1); min-width: 0; }
.result-card__title h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); overflow-wrap: anywhere; }
.result-card__meta { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.result-card__outcome { display: grid; justify-items: end; gap: 6px; flex-shrink: 0; }
.result-card__summary { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.result-link { color: inherit; text-decoration: none; }
.result-link:hover, .result-link:focus-visible { color: var(--md-sys-color-primary); text-decoration: underline; }
.result-score { color: var(--md-sys-color-primary); font: var(--md-sys-headline-small); white-space: nowrap; }
.result-score span { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-title-medium); }
.outcome-chip { width: fit-content; padding: var(--space-1) 10px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-container-highest); color: var(--md-sys-color-on-surface); font: var(--md-sys-label-medium); white-space: nowrap; }
.outcome-chip--scored { background: var(--md-sys-color-primary-container); color: var(--md-sys-color-on-primary-container); }
.outcome-chip--scored-zero { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.outcome-chip--not-completed { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.outcome-chip--pending { background: var(--md-sys-color-surface-container-high); color: var(--md-sys-color-primary); }
.load-more { justify-self: start; margin-top: 4px; }
.load-more-error { margin-top: 8px; }
.outlined-button { min-height: 40px; padding: 0 var(--space-6); border: 1px solid var(--md-sys-color-outline); border-radius: var(--md-sys-shape-full); background: transparent; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); cursor: pointer; }
@media (max-width: 600px) {
  .result-card__head { flex-direction: column; }
  .result-card__outcome { justify-items: start; }
}
</style>
