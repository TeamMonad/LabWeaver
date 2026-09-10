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
              <section v-if="step.review" class="goal-review" aria-label="目标建议">
                <div class="goal-review__heading">
                  <h4>GoalReview · 目标建议</h4>
                  <span class="review-assessment">{{ assessmentLabel(step.review.assessment) }}</span>
                </div>
                <div class="review-meta">
                  <span>置信度</span>
                  <strong>{{ formatConfidence(step.review.confidence) }}</strong>
                  <span>Schema</span>
                  <code>{{ step.review.schema_version }}</code>
                </div>
                <p class="review-disclaimer">这是评测建议，不计入确定性总分。</p>
                <DiagnosticBanner
                  v-if="step.review.requires_teacher_attention"
                  code="GOAL_REVIEW_TEACHER_ATTENTION"
                  message="该建议需要教师关注，请结合提交内容和证据人工复核。"
                  :retryable="false"
                  severity="warning"
                />
                <ol v-if="step.review.findings.length > 0" class="finding-list">
                  <li v-for="(finding, findingIndex) in step.review.findings" :key="`${findingIndex}-${finding.criterion}`" class="finding-item">
                    <div class="finding-heading">
                      <strong>{{ finding.criterion }}</strong>
                      <span>{{ findingResultLabel(finding.result) }}</span>
                    </div>
                    <p>{{ finding.suggestion }}</p>
                    <ul class="evidence-list">
                      <li v-for="(location, locationIndex) in finding.evidence" :key="`${locationIndex}-${location.path}:${location.start_line}-${location.end_line}`">
                        证据：<code>{{ location.path }}:{{ location.start_line }}-{{ location.end_line }}</code>
                      </li>
                    </ul>
                  </li>
                </ol>
              </section>
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
import { useProjects } from '@/composables/useProjects'
import { useEvaluationResult } from '@/composables/useEvaluationResults'
import type { StudentEvaluationResultSchema, StudentEvaluationStepResult } from '@/generated/contracts'
import { formatTimestamp } from '@/utils/format'

const route = useRoute()
const projects = useProjects()
const projectId = computed(() => projects.selectedProjectId ?? undefined)
const runId = computed(() => typeof route.params.runId === 'string' ? route.params.runId : undefined)
const evaluation = useEvaluationResult(projectId, runId)

function stateLabel(state: StudentEvaluationResultSchema['state']) {
  return { succeeded: '成功', failed: '失败', cancelled: '已取消' }[state] ?? state
}
function roleLabel(role: StudentEvaluationStepResult['role']) {
  return { gate: '门禁', score: '评分', advisory: '建议' }[role]
}
function assessmentLabel(assessment: NonNullable<StudentEvaluationStepResult['review']>['assessment']) {
  return { met: '目标已达成', partially_met: '目标部分达成', not_met: '目标未达成', insufficient_evidence: '证据不足' }[assessment]
}
function findingResultLabel(result: NonNullable<StudentEvaluationStepResult['review']>['findings'][number]['result']) {
  return { met: '符合', partial: '部分符合', missing: '缺少证据', unclear: '需要澄清' }[result]
}
function formatConfidence(confidence: number) {
  return `${Math.round(confidence * 100)}%`
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
.goal-review { grid-column: 1 / -1; display: grid; gap: 12px; margin-top: 4px; padding-top: 14px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.goal-review__heading, .finding-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.goal-review h4 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.review-assessment { padding: 4px 10px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); font: var(--md-sys-label-medium); }
.review-meta { display: flex; flex-wrap: wrap; gap: 8px 14px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.review-meta strong, .review-meta code { color: var(--md-sys-color-on-surface); }
.review-disclaimer { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.finding-list { display: grid; gap: 10px; margin: 0; padding-left: 20px; }
.finding-item { display: grid; gap: 6px; padding: 10px 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.finding-item p { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.finding-heading span { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.evidence-list { display: grid; gap: 4px; margin: 0; padding-left: 18px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.evidence-list code { color: var(--md-sys-color-on-surface); }
@media (max-width: 600px) { .summary dl, .step-card { grid-template-columns: 1fr; } .step-card code { grid-column: auto; } }
</style>
