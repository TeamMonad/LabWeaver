<template>
  <section class="result-page" aria-labelledby="result-heading">
    <RouterLink class="back-link" :to="{ path: '/student/results', query: { projectId: routeProjectId } }">
      <SvgIcon name="arrow_back" size="sm" aria-hidden="true" />
      <span>返回评测结果</span>
    </RouterLink>

    <AsyncStateView :state="evaluation.result" @retry="evaluation.load()">
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
      <template #empty>
        <div v-if="runProgress && runReading" class="result-body" data-testid="evaluation-in-progress">
          <header class="result-header">
            <p class="eyebrow">评测进行中</p>
            <h1 id="result-heading">{{ title }}</h1>
            <p v-if="hintedSubmission" class="result-header__meta">提交于 {{ formatTimestamp(hintedSubmission.frozenAt) }}</p>
          </header>

          <article class="outcome-card md-card outcome-card--pending" aria-labelledby="outcome-heading">
            <div class="outcome-card__main">
              <span class="outcome-chip outcome-chip--pending">{{ runReading.label }}</span>
              <h2 id="outcome-heading" class="outcome-card__summary">{{ runReading.detail }}</h2>
              <p v-if="runReading.status === 'failed' && runReading.reason" class="outcome-card__reason">原因：{{ runReading.reason }}。</p>
              <p class="outcome-card__next">评测结束前不显示分数。页面会自动更新，也可以离开后再回来查看。</p>
            </div>
          </article>

          <section class="detail-section md-card" aria-labelledby="progress-heading">
            <h2 id="progress-heading">提交进度</h2>
            <SubmissionProgress :stages="runStages" />
            <SubmissionTaskNotes :record="runTask.record" />
          </section>

          <CollapsibleRoot class="technical md-card">
            <CollapsibleTrigger class="technical__trigger">
              <SvgIcon name="chevron_right" size="sm" aria-hidden="true" class="technical__chevron" />
              <span>技术信息（反馈问题时提供）</span>
            </CollapsibleTrigger>
            <CollapsibleContent>
              <dl class="technical__list">
                <dt>评测编号</dt><dd><code>{{ runProgress.runId }}</code></dd>
                <template v-if="runReading.diagnosticCode">
                  <dt>诊断码</dt><dd><code>{{ runReading.diagnosticCode }}</code></dd>
                </template>
              </dl>
              <CopyButton :text="progressTechnicalText" label="复制技术信息" aria-label="复制评测编号和诊断码" />
            </CollapsibleContent>
          </CollapsibleRoot>
        </div>
        <DiagnosticBanner
          v-else-if="runTask.record.readError"
          :code="runTask.record.readError.code"
          :message="runMissingMessage"
          :retryable="runTask.record.readError.retryable"
          @retry="runTask.refresh()"
        />
        <div v-else class="state-message" role="status">正在读取评测进度…</div>
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
import SubmissionTaskNotes from '@/components/evaluation/SubmissionTaskNotes.vue'
import { useRunTask, useSubmissionTasks } from '@/composables/useEvaluationTasks'
import { useProjects } from '@/composables/useProjects'
import { useEvaluationResult } from '@/composables/useEvaluationResults'
import { useSubmissionDirectory } from '@/composables/useSubmissionDirectory'
import { useVisiblePolling } from '@/composables/useVisiblePolling'
import type { StudentEvaluationResultSchema, StudentEvaluationStepResult } from '@/generated/contracts'
import {
  evaluationOutcome,
  evaluationReason,
  evaluationStepRoleLabel,
  evaluationStepStateLabel,
} from '@/utils/evaluationOutcome'
import { isTerminalRun, readEvaluationTask, submissionStages } from '@/utils/evaluationProgress'
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

// A run without a terminal result is read through its task progress.
const runTask = useRunTask(projectId, runId)
const notFinished = computed(() => evaluation.result.kind === 'empty')
const runProgress = computed(() => runTask.record.fact.kind === 'progress' ? runTask.record.fact.progress : null)
const runReading = computed(() => runProgress.value ? readEvaluationTask(runProgress.value) : null)

// The progress read model carries no submission, so a submission named by the
// link that led here is used only after the server maps it to this same run.
const submissionHint = computed(() => typeof route.query.submissionId === 'string' ? route.query.submissionId : undefined)
const hintTasks = useSubmissionTasks(projectId)
const verifiedHint = computed(() => {
  const hint = submissionHint.value
  const fact = hint ? hintTasks.records[hint]?.fact : undefined
  return hint && fact?.kind === 'progress' && fact.progress.runId === runId.value ? hint : null
})

function readySubmission(id: string | null | undefined) {
  const record = id ? directory.records[id] : undefined
  return record?.kind === 'ready' ? record : null
}
const submission = computed(() => readySubmission(result.value?.frozenSubmissionId))
const hintedSubmission = computed(() => readySubmission(verifiedHint.value))
const title = computed(() => (submission.value ?? hintedSubmission.value)?.environmentLabel ?? '实验提交')
const stages = computed(() => submissionStages({ frozenAt: submission.value?.frozenAt ?? null, result: result.value }, formatTimestamp))
const runStages = computed(() => submissionStages(
  { frozenAt: hintedSubmission.value?.frozenAt ?? null, task: runTask.record.fact },
  formatTimestamp,
))
const runMissingMessage = computed(() => runTask.record.readError?.status === 404
  ? '找不到这次评测。它可能不属于当前账号，或不在所选项目中。请返回评测结果页查看。'
  : runTask.record.readError?.message ?? '')
const progressTechnicalText = computed(() => [
  `评测编号：${runProgress.value?.runId ?? runId.value ?? ''}`,
  ...(runReading.value?.diagnosticCode ? [`诊断码：${runReading.value.diagnosticCode}`] : []),
].join('\n'))

// The page title names the experiment once it is known, not the run identifier.
watch([title, notFinished], ([value]) => {
  if (typeof document === 'undefined') return
  if (result.value) document.title = `${value} · 评测详情 · LabWeaver`
  else if (notFinished.value) document.title = `${value} · 评测进行中 · LabWeaver`
})

watch(() => result.value?.frozenSubmissionId, (id) => { if (id) void directory.resolve([id]) }, { immediate: true })
watch(notFinished, (value) => {
  if (!value) return
  void runTask.refresh()
  if (submissionHint.value) void hintTasks.refresh([submissionHint.value])
}, { immediate: true })
watch(verifiedHint, (id) => { if (id) void directory.resolve([id]) })

// While the run has no result: re-read its progress, and once the run has
// ended, re-read the result instead. Polling stops when the result arrives or
// when the server says the run does not exist for this account.
const runMissing = computed(() => runTask.record.fact.kind !== 'progress' && runTask.record.readError?.status === 404)
useVisiblePolling(async () => {
  if (runProgress.value && isTerminalRun(runProgress.value)) await evaluation.load({ quiet: true })
  else await runTask.refresh()
}, computed(() => notFinished.value && !runMissing.value))
watch(() => runProgress.value && isTerminalRun(runProgress.value), (ended) => {
  if (ended && notFinished.value) void evaluation.load({ quiet: true })
})

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
.result-page { display: flex; flex-direction: column; gap: var(--space-5); max-width: var(--content-max-width); margin: 0 auto; }
.result-body { display: flex; flex-direction: column; gap: var(--space-5); }
.back-link { display: inline-flex; align-items: center; gap: var(--space-1); width: fit-content; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); text-decoration: none; }
.result-header { display: grid; gap: var(--space-1); }
.eyebrow { margin: 0; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); }
.result-header h1 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-headline-small); overflow-wrap: anywhere; }
.result-header__meta, .section-hint { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.outcome-card { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: var(--space-3) var(--space-6); align-items: start; padding: var(--space-5); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-large); }
.outcome-card__main { display: grid; gap: 6px; }
.outcome-card__summary { margin: var(--space-1) 0 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.outcome-card__reason, .outcome-card__next { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.outcome-card__reason { color: var(--md-sys-color-on-surface); }
.outcome-card__score { text-align: right; white-space: nowrap; }
.outcome-card__score strong { color: var(--md-sys-color-primary); font: var(--md-sys-display-small, var(--md-sys-headline-large)); }
.outcome-card__score span { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-title-large); }
.outcome-card__score--none strong { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-title-large); }
.outcome-card__diagnostic { grid-column: 1 / -1; }
.outcome-chip { width: fit-content; padding: var(--space-1) 10px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-container-highest); font: var(--md-sys-label-medium); }
.outcome-chip--scored { background: var(--md-sys-color-primary-container); color: var(--md-sys-color-on-primary-container); }
.outcome-chip--scored-zero { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.outcome-chip--not-completed { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.outcome-chip--pending { background: var(--md-sys-color-surface-container-high); color: var(--md-sys-color-primary); }
.state-message { margin: 0; padding: var(--space-8) 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); text-align: center; }
.detail-section { display: grid; gap: var(--space-3); }
.detail-section.md-card { padding: var(--space-4) var(--space-5); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.detail-section h2 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.step-list { display: grid; gap: 10px; margin: 0; padding: 0; list-style: none; }
.step-card { display: grid; gap: var(--space-2); padding: 14px var(--space-4); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.step-card__head { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2) var(--space-3); }
.step-card__index { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.step-card__role { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.step-card__score { margin-left: auto; color: var(--md-sys-color-primary); font: var(--md-sys-title-medium); }
.step-card__reason { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.step-state { padding: 2px var(--space-2); border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-container-high); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.step-state--succeeded { color: var(--md-sys-color-primary); }
.step-state--failed { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.goal-review { display: grid; gap: var(--space-2); margin-top: var(--space-1); padding-top: 12px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.goal-review__heading, .finding-heading { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); }
.goal-review h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.review-disclaimer, .review-attention { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.review-attention { color: var(--md-sys-color-on-surface); }
.finding-list { display: grid; gap: var(--space-2); margin: var(--space-2) 0 0; padding: 0; list-style: none; }
.finding-item { display: grid; gap: var(--space-1); padding: 10px var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.finding-item p { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.finding-heading span { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.evidence-list { display: grid; gap: 2px; margin: 0; padding-left: 18px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.text-button { border: none; background: transparent; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); cursor: pointer; }
.technical { padding: var(--space-1) var(--space-2); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.technical__trigger { display: inline-flex; align-items: center; gap: var(--space-1); width: 100%; min-height: 40px; padding: 0 var(--space-2); border: none; background: transparent; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-large); text-align: left; cursor: pointer; }
.technical__chevron { transition: transform 160ms cubic-bezier(0.2, 0, 0, 1); }
.technical__trigger[data-state='open'] .technical__chevron { transform: rotate(90deg); }
.technical__list { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 6px var(--space-4); margin: var(--space-1) var(--space-2) var(--space-3); font: var(--md-sys-body-small); }
.technical__list dt { color: var(--md-sys-color-on-surface-variant); }
.technical__list dd { margin: 0; overflow-wrap: anywhere; }
.technical :deep(.copy-button) { margin: 0 var(--space-2) var(--space-3); }
@media (prefers-reduced-motion: reduce) { .technical__chevron { transition: none; } }
@media (max-width: 600px) {
  .outcome-card { grid-template-columns: 1fr; }
  .outcome-card__score { text-align: left; }
}
</style>
