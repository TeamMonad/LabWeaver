<template>
  <section class="results-page" aria-labelledby="results-heading">
    <header>
      <h2 id="results-heading">评测结果</h2>
      <p>仅展示当前课程中属于你的终态 EvaluationRun；失败或取消不会显示部分总分。</p>
    </header>

    <AsyncStateView :state="evaluation.results" empty-text="当前课程暂无终态评测结果" @retry="evaluation.load()">
      <template #success="{ data: results }">
        <ul class="result-list">
          <li v-for="result in results" :key="result.runId" class="result-card md-card">
            <div class="result-main">
              <span class="state-chip" :class="`state-chip--${result.state}`">{{ stateLabel(result.state) }}</span>
              <RouterLink class="result-link" :to="`/student/results/${result.runId}`">
                <code>{{ result.runId }}</code>
              </RouterLink>
              <span class="result-time">完成于 {{ formatTimestamp(result.completedAt) }}</span>
            </div>
            <strong v-if="result.state === 'succeeded'" class="result-score">
              {{ result.awardedScore }} / {{ result.maxScore }}
            </strong>
            <DiagnosticBanner
              v-else
              :code="result.diagnosticCode ?? fallbackDiagnostic(result.state)"
              :message="result.state === 'failed' ? '评测失败，未产生可发布的总分。' : '评测已取消，未产生可发布的总分。'"
              :retryable="false"
              severity="warning"
            />
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
      </template>
    </AsyncStateView>
  </section>
</template>

<script setup lang="ts">
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import { useCourseContext } from '@/composables/useCourseContext'
import { useEvaluationResults } from '@/composables/useEvaluationResults'
import type { StudentEvaluationResultSchema } from '@/generated/contracts'
import { formatTimestamp } from '@/utils/format'

const course = useCourseContext()
const evaluation = useEvaluationResults(course.courseId)

function stateLabel(state: StudentEvaluationResultSchema['state']) {
  return { succeeded: '成功', failed: '失败', cancelled: '已取消' }[state] ?? state
}

function fallbackDiagnostic(state: StudentEvaluationResultSchema['state']) {
  return state === 'failed' ? 'LW_EVALUATION_RUN_FAILED' : 'LW_EVALUATION_RUN_CANCELLED'
}
</script>

<style scoped>
.results-page { display: flex; flex-direction: column; gap: 20px; }
h2 { margin: 0; font: var(--md-sys-headline-small); }
header p { margin: 4px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.result-list { display: grid; gap: 12px; list-style: none; margin: 0; padding: 0; }
.result-card { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 16px; align-items: center; padding: 16px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.result-main { display: flex; flex-direction: column; gap: 8px; min-width: 0; }
.result-link { color: var(--md-sys-color-primary); overflow-wrap: anywhere; }
.result-time { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.result-score { color: var(--md-sys-color-primary); font: var(--md-sys-headline-small); white-space: nowrap; }
.state-chip { width: fit-content; padding: 4px 10px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-medium); background: var(--md-sys-color-surface-container-highest); }
.state-chip--succeeded { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.state-chip--failed { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
.load-more { margin-top: 16px; }
.outlined-button { min-height: 40px; padding: 0 24px; border: 1px solid var(--md-sys-color-outline); border-radius: var(--md-sys-shape-full); background: transparent; color: var(--md-sys-color-primary); cursor: pointer; }
@media (max-width: 600px) { .result-card { grid-template-columns: 1fr; } }
</style>
