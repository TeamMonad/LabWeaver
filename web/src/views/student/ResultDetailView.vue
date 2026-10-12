<template>
  <section class="result-page" aria-labelledby="result-heading">
    <RouterLink class="back-link" :to="{ path: '/student/results', query: { projectId: routeProjectId } }">
      <SvgIcon name="arrow_back" size="sm" aria-hidden="true" />
      <span>返回评测结果</span>
    </RouterLink>

    <AsyncStateView :state="evaluation.result" @retry="evaluation.load">
      <template #success="{ data: result }">
        <div class="result-body">
        <header class="result-header">
          <p class="eyebrow">评测详情</p>
          <h1 id="result-heading">{{ title }}</h1>
          <p class="result-header__meta">
            <template v-if="submission">提交于 {{ formatTimestamp(submission.frozenAt) }} · </template>
            完成于 {{ formatTimestamp(result.completedAt) }}
          </p>
        </header>

        <article
          class="outcome-card md-card"
          :class="`outcome-card--${outcome!.kind}`"
          aria-labelledby="outcome-heading"
          data-testid="evaluation-outcome"
        >
          <div class="outcome-card__main">
            <span class="outcome-chip" :class="`outcome-chip--${outcome!.kind}`">{{ outcome!.label }}</span>
            <h2 id="outcome-heading" class="outcome-card__summary">{{ outcome!.summary }}</h2>
            <p v-if="outcome!.reason" class="outcome-card__reason">原因：{{ outcome!.reason }}。</p>
            <p class="outcome-card__next">{{ outcome!.nextStep }}</p>
          </div>
          <div v-if="outcome!.score" class="outcome-card__score" aria-label="最终总分">
            <strong>{{ outcome!.score.awarded }}</strong><span> / {{ outcome!.score.max }}</span>
          </div>
          <div v-else class="outcome-card__score outcome-card__score--none">
            <strong>不计分</strong>
          </div>
          <DiagnosticBanner
            v-if="outcome!.kind === 'not-completed' || outcome!.kind === 'cancelled'"
            class="outcome-card__diagnostic"
            :code="result.diagnosticCode ?? fallbackDiagnostic(result.state)"
            message="向教师或管理员反馈时，请提供这个诊断码。"
            :retryable="false"
            severity="warning"
          />
        </article>

        <section class="detail-section md-card" aria-labelledby="progress-heading">
          <h2 id="progress-heading">提交进度</h2>
          <SubmissionProgress :stages="stages" />
        </section>

        <section class="detail-section" aria-labelledby="steps-heading">
          <h2 id="steps-heading">评测步骤</h2>
          <p class="section-hint">
            总分只由前置检查和评分步骤确定；学习建议不计入总分。
          </p>
          <ol class="step-list">
            <li
              v-for="step in result.steps"
              :key="step.position"
              class="step-card md-card"
              :class="`step-card--${step.state}`"
            >
              <div class="step-card__head">
                <span class="step-card__index">步骤 {{ step.position + 1 }}</span>
                <strong class="step-card__role">{{ evaluationStepRoleLabel(step.role) }}</strong>
                <span class="step-state" :class="`step-state--${step.state}`">{{ evaluationStepStateLabel(step, outcome!.kind) }}</span>
                <span v-if="stepScore(step)" class="step-card__score">{{ stepScore(step) }}</span>
              </div>
              <p v-if="evaluationReason(step.diagnosticCode)" class="step-card__reason">
                {{ evaluationReason(step.diagnosticCode) }}
              </p>

              <CollapsibleRoot v-if="step.review" v-slot="{ open }" class="goal-review" :default-open="true">
                <div class="goal-review__heading">
                  <h3>学习建议 · {{ assessmentLabel(step.review.assessment) }}</h3>
                  <CollapsibleTrigger class="text-button">
                    {{ open ? '收起' : '展开' }}
                  </CollapsibleTrigger>
                </div>
                <p class="review-disclaimer">
                  这是根据本次提交内容给出的建议，不计入总分。置信度 {{ formatConfidence(step.review.confidence) }}。
                </p>
                <CollapsibleContent>
                  <p v-if="step.review.requires_teacher_attention" class="review-attention">
                    这条建议需要教师结合提交内容人工复核。
                  </p>
                  <ol v-if="step.review.findings.length > 0" class="finding-list">
                    <li
                      v-for="(finding, findingIndex) in step.review.findings"
                      :key="`${findingIndex}-${finding.criterion}`"
                      class="finding-item"
                    >
                      <div class="finding-heading">
                        <strong>{{ finding.criterion }}</strong>
                        <span>{{ findingResultLabel(finding.result) }}</span>
                      </div>
                      <p>{{ finding.suggestion }}</p>
                      <ul v-if="finding.evidence.length" class="evidence-list">
                        <li
                          v-for="(location, locationIndex) in finding.evidence"
                          :key="`${locationIndex}-${location.path}:${location.start_line}-${location.end_line}`"
                        >
                          依据：<code>{{ location.path }}</code> 第 {{ location.start_line }}–{{ location.end_line }} 行
                        </li>
                      </ul>
                    </li>
                  </ol>
                </CollapsibleContent>
              </CollapsibleRoot>
            </li>
          </ol>
        </section>

        <CollapsibleRoot class="technical md-card">
          <CollapsibleTrigger class="technical__trigger">
            <SvgIcon name="chevron_right" size="sm" aria-hidden="true" class="technical__chevron" />
            <span>技术信息（反馈问题时提供）</span>
          </CollapsibleTrigger>
          <CollapsibleContent>
            <dl class="technical__list">
              <dt>评测编号</dt><dd><code>{{ result.runId }}</code></dd>
              <dt>提交编号</dt><dd><code>{{ result.frozenSubmissionId }}</code></dd>
              <template v-if="result.diagnosticCode">
                <dt>诊断码</dt><dd><code>{{ result.diagnosticCode }}</code></dd>
              </template>
              <template v-for="step in stepsWithCode(result.steps)" :key="`code-${step.position}`">
                <dt>步骤 {{ step.position + 1 }} 诊断码</dt><dd><code>{{ step.diagnosticCode }}</code></dd>
              </template>
            </dl>
            <CopyButton :text="technicalText(result)" label="复制技术信息" aria-label="复制评测编号、提交编号和诊断码" />
          </CollapsibleContent>
        </CollapsibleRoot>
        </div>
      </template>
    </AsyncStateView>
  </section>
</template>

<script setup lang="ts">
import { computed, watch } from 'vue'
import { useRoute } from 'vue-router'
import { CollapsibleContent, CollapsibleRoot, CollapsibleTrigger } from 'reka-ui'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import CopyButton from '@/components/common/CopyButton.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import SubmissionProgress from '@/components/evaluation/SubmissionProgress.vue'
import { useProjects } from '@/composables/useProjects'
import { useEvaluationResult } from '@/composables/useEvaluationResults'
import { useSubmissionDirectory } from '@/composables/useSubmissionDirectory'
import type { StudentEvaluationResultSchema, StudentEvaluationStepResult } from '@/generated/contracts'
import {
  evaluationOutcome,
  evaluationReason,
  evaluationStepRoleLabel,
  evaluationStepStateLabel,
  submissionStages,
} from '@/utils/evaluationOutcome'
import { formatTimestamp } from '@/utils/format'

const route = useRoute()
const projects = useProjects()
const routeProjectId = computed(() => {
  const id = typeof route.query.projectId === 'string' ? route.query.projectId.trim() : ''
  return id || undefined
})
const projectId = computed(() => routeProjectId.value ?? projects.selectedProjectId ?? undefined)
const runId = computed(() => typeof route.params.runId === 'string' ? route.params.runId : undefined)
const evaluation = useEvaluationResult(projectId, runId)
const directory = useSubmissionDirectory(projectId)

const result = computed(() => evaluation.result.kind === 'success' ? evaluation.result.data : null)
const outcome = computed(() => result.value ? evaluationOutcome(result.value) : null)
const submission = computed(() => {
  const id = result.value?.frozenSubmissionId
  const record = id ? directory.records[id] : undefined
  return record?.kind === 'ready' ? record : null
})
const title = computed(() => submission.value?.environmentLabel ?? '实验提交')
const stages = computed(() => submissionStages({ frozenAt: submission.value?.frozenAt ?? null, result: result.value }, formatTimestamp))

// The page title names the experiment once it is known, not the run identifier.
watch(title, (value) => {
  if (typeof document !== 'undefined' && result.value) document.title = `${value} · 评测详情 · LabWeaver`
})

watch(() => result.value?.frozenSubmissionId, (id) => { if (id) void directory.resolve([id]) }, { immediate: true })

function stepScore(step: StudentEvaluationStepResult): string | null {
  // Step scores are shown only when the whole run produced a final score.
  if (outcome.value?.score === null || outcome.value?.score === undefined) return null
  if (step.role !== 'score' || typeof step.awardedScore !== 'number') return null
  return `${step.awardedScore} / ${step.maxScore}`
}
function stepsWithCode(steps: StudentEvaluationStepResult[]) {
  return steps.filter((step) => step.diagnosticCode)
}
function technicalText(value: StudentEvaluationResultSchema) {
  return [
    `评测编号：${value.runId}`,
    `提交编号：${value.frozenSubmissionId}`,
    ...(value.diagnosticCode ? [`诊断码：${value.diagnosticCode}`] : []),
    ...stepsWithCode(value.steps).map((step) => `步骤 ${step.position + 1} 诊断码：${step.diagnosticCode}`),
  ].join('\n')
}
function assessmentLabel(assessment: NonNullable<StudentEvaluationStepResult['review']>['assessment']) {
  return { met: '目标已达成', partially_met: '目标部分达成', not_met: '目标未达成', insufficient_evidence: '证据不足' }[assessment]
}
function findingResultLabel(value: NonNullable<StudentEvaluationStepResult['review']>['findings'][number]['result']) {
  return { met: '符合', partial: '部分符合', missing: '缺少', unclear: '需要澄清' }[value]
}
function formatConfidence(confidence: number) {
  return `${Math.round(confidence * 100)}%`
}
function fallbackDiagnostic(state: StudentEvaluationResultSchema['state']) {
  return state === 'cancelled' ? 'LW_EVALUATION_RUN_CANCELLED' : 'LW_EVALUATION_RUN_FAILED'
}
</script>

<style scoped>
.result-page { display: flex; flex-direction: column; gap: 20px; max-width: var(--content-max-width); margin: 0 auto; }
.result-body { display: flex; flex-direction: column; gap: 20px; }
.back-link { display: inline-flex; align-items: center; gap: 4px; width: fit-content; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); text-decoration: none; }
.result-header { display: grid; gap: 4px; }
.eyebrow { margin: 0; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); }
.result-header h1 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-headline-small); overflow-wrap: anywhere; }
.result-header__meta, .section-hint { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.outcome-card { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 12px 24px; align-items: start; padding: 20px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-large); }
.outcome-card__main { display: grid; gap: 6px; }
.outcome-card__summary { margin: 4px 0 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.outcome-card__reason, .outcome-card__next { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.outcome-card__reason { color: var(--md-sys-color-on-surface); }
.outcome-card__score { text-align: right; white-space: nowrap; }
.outcome-card__score strong { color: var(--md-sys-color-primary); font: var(--md-sys-display-small, var(--md-sys-headline-large)); }
.outcome-card__score span { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-title-large); }
.outcome-card__score--none strong { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-title-large); }
.outcome-card__diagnostic { grid-column: 1 / -1; }
.outcome-chip { width: fit-content; padding: 4px 10px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-container-highest); font: var(--md-sys-label-medium); }
.outcome-chip--scored { background: var(--md-sys-color-primary-container); color: var(--md-sys-color-on-primary-container); }
.outcome-chip--scored-zero { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.outcome-chip--not-completed { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.detail-section { display: grid; gap: 12px; }
.detail-section.md-card { padding: 16px 20px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.detail-section h2 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.step-list { display: grid; gap: 10px; margin: 0; padding: 0; list-style: none; }
.step-card { display: grid; gap: 8px; padding: 14px 16px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.step-card__head { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 12px; }
.step-card__index { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.step-card__role { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.step-card__score { margin-left: auto; color: var(--md-sys-color-primary); font: var(--md-sys-title-medium); }
.step-card__reason { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.step-state { padding: 2px 8px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-container-high); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.step-state--succeeded { color: var(--md-sys-color-primary); }
.step-state--failed { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.goal-review { display: grid; gap: 8px; margin-top: 4px; padding-top: 12px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.goal-review__heading, .finding-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.goal-review h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.review-disclaimer, .review-attention { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.review-attention { color: var(--md-sys-color-on-surface); }
.finding-list { display: grid; gap: 8px; margin: 8px 0 0; padding: 0; list-style: none; }
.finding-item { display: grid; gap: 4px; padding: 10px 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.finding-item p { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.finding-heading span { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.evidence-list { display: grid; gap: 2px; margin: 0; padding-left: 18px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.text-button { border: none; background: transparent; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); cursor: pointer; }
.technical { padding: 4px 8px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.technical__trigger { display: inline-flex; align-items: center; gap: 4px; width: 100%; min-height: 40px; padding: 0 8px; border: none; background: transparent; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-large); text-align: left; cursor: pointer; }
.technical__chevron { transition: transform 160ms cubic-bezier(0.2, 0, 0, 1); }
.technical__trigger[data-state='open'] .technical__chevron { transform: rotate(90deg); }
.technical__list { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 6px 16px; margin: 4px 8px 12px; font: var(--md-sys-body-small); }
.technical__list dt { color: var(--md-sys-color-on-surface-variant); }
.technical__list dd { margin: 0; overflow-wrap: anywhere; }
.technical :deep(.copy-button) { margin: 0 8px 12px; }
@media (prefers-reduced-motion: reduce) { .technical__chevron { transition: none; } }
@media (max-width: 600px) {
  .outcome-card { grid-template-columns: 1fr; }
  .outcome-card__score { text-align: left; }
}
</style>
