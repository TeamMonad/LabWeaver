<template>
  <section class="result-page" aria-labelledby="result-heading">
    <RouterLink to="/student/results" class="back-link">← 返回评测结果</RouterLink>
    <h2 id="result-heading">评测详情</h2>
    <AsyncStateView :state="evaluation.result" @retry="evaluation.load">
      <template #success="{ data: result }">
        <article class="summary md-card">
          <dl>
            <dt>Run</dt><dd><code>{{ result.runId }}</code></dd>
            <dt>状态</dt><dd>{{ stateLabel(result.state) }}</dd>
            <dt>完成时间</dt><dd>{{ formatTimestamp(result.completedAt) }}</dd>
            <template v-if="result.state === 'succeeded'">
              <dt>最终总分</dt><dd class="score">{{ result.awardedScore }} / {{ result.maxScore }}</dd>
            </template>
          </dl>
          <DiagnosticBanner
            v-if="result.state !== 'succeeded'"
            :code="result.diagnosticCode ?? fallbackDiagnostic(result.state)"
            message="本次评测未产生可发布的最终总分；页面不会展示部分分数。"
            :retryable="false"
            severity="warning"
          />
        </article>

        <section aria-labelledby="steps-heading">
          <h3 id="steps-heading">公开步骤</h3>
          <ol class="step-list">
            <li v-for="step in result.steps" :key="step.position" class="step-card md-card">
              <span>步骤 {{ step.position + 1 }}</span>
              <span>{{ roleLabel(step.role) }} · {{ step.state }}</span>
              <strong v-if="result.state === 'succeeded' && step.awardedScore !== undefined && step.awardedScore !== null">
                {{ step.awardedScore }} / {{ step.maxScore }}
              </strong>
              <code v-if="step.diagnosticCode">{{ step.diagnosticCode }}</code>
            </li>
          </ol>
        </section>
      </template>
    </AsyncStateView>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import { useCourseContext } from '@/composables/useCourseContext'
import { useEvaluationResult } from '@/composables/useEvaluationResults'
import type { StudentEvaluationResultSchema, StudentEvaluationStepResult } from '@/generated/contracts'
import { formatTimestamp } from '@/utils/format'

const route = useRoute()
const course = useCourseContext()
const runId = computed(() => typeof route.params.runId === 'string' ? route.params.runId : undefined)
const evaluation = useEvaluationResult(course.courseId, runId)

function stateLabel(state: StudentEvaluationResultSchema['state']) {
  return { succeeded: '成功', failed: '失败', cancelled: '已取消' }[state] ?? state
}
function roleLabel(role: StudentEvaluationStepResult['role']) {
  return { gate: '门禁', score: '评分', advisory: '建议' }[role]
}
function fallbackDiagnostic(state: StudentEvaluationResultSchema['state']) {
  return state === 'failed' ? 'LW_EVALUATION_RUN_FAILED' : 'LW_EVALUATION_RUN_CANCELLED'
}
</script>

<style scoped>
.result-page { display: flex; flex-direction: column; gap: 20px; }
.back-link { color: var(--md-sys-color-primary); }
h2, h3 { margin: 0; color: var(--md-sys-color-on-surface); }
.summary { padding: 16px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.summary dl { display: grid; grid-template-columns: 140px 1fr; gap: 10px 16px; margin: 0 0 12px; }
.summary dt { color: var(--md-sys-color-on-surface-variant); }
.summary dd { min-width: 0; margin: 0; overflow-wrap: anywhere; }
.score { color: var(--md-sys-color-primary); font: var(--md-sys-title-large); }
.step-list { display: grid; gap: 10px; padding-left: 0; list-style: none; }
.step-card { display: grid; grid-template-columns: 100px minmax(120px, 1fr) auto; gap: 12px; padding: 14px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.step-card code { grid-column: 2 / -1; overflow-wrap: anywhere; }
@media (max-width: 600px) { .summary dl, .step-card { grid-template-columns: 1fr; } .step-card code { grid-column: auto; } }
</style>
