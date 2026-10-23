<template>
  <div class="tab-body">
                  <div class="freeze-section md-card">
                    <h4 class="section-subtitle">
                      提交评测
                    </h4>
                    <p class="freeze-desc">
                      确认要提交的文件内容后，平台会保存不可变快照并生成评测结果。
                    </p>
                    <div class="freeze-action-row">
                      <button
                        type="button"
                        class="filled-button"
                        :disabled="!canFreeze(data) || freezeState.kind === 'loading'"
                        @click="freezeConfirmVisible = true"
                      >
                        {{ freezeState.kind === 'loading' ? '冻结中…' : '发起冻结提交' }}
                      </button>
                    </div>

                    <div
                      v-if="freezeStatus"
                      class="freeze-status-card"
                      role="status"
                      aria-live="polite"
                    >
                      <div class="freeze-status-header">
                        <span class="freeze-status-label">冻结提交状态</span>
                        <strong>{{ freezeStatusLabel(freezeStatus) }}</strong>
                      </div>
                      <p>{{ freezeStatusDescription(freezeStatus) }}</p>
                      <p
                        v-if="freezeOperation?.diagnosticCode"
                        class="freeze-status-diagnostic"
                      >
                        诊断码：<code>{{ freezeOperation.diagnosticCode }}</code>
                      </p>
                      <div class="freeze-status-actions">
                        <RouterLink
                          v-if="freezeStatus === 'accepted' || freezeStatus === 'running' || freezeStatus === 'succeeded'"
                          :to="{ path: '/student/results', query: { projectId: data.projectId } }"
                          class="outlined-button"
                        >
                          查看评测结果
                        </RouterLink>
                        <button
                          v-if="freezeOperation?.state === 'failed' && freezeOperation.retryEligible"
                          type="button"
                          class="outlined-button"
                          :disabled="freezeState.kind === 'loading'"
                          @click="retryFreeze"
                        >
                          重试冻结提交
                        </button>
                      </div>
                    </div>

                    <p
                      v-if="freezeBlockReason(data)"
                      class="freeze-unavailable"
                      role="status"
                    >
                      {{ freezeBlockReason(data) }}
                    </p>

                    <div
                      v-if="freezeConfirmVisible"
                      class="freeze-confirm"
                      role="dialog"
                      aria-label="确认冻结清单"
                    >
                      <h5 class="section-subtitle">
                        确认提交内容
                      </h5>
                      <template v-if="selectedSubmissionManifestFor(data)">
                        <div class="freeze-manifest">
                          <div class="freeze-manifest-group">
                            <strong>收集文件</strong>
                            <ul>
                              <li
                                v-for="rule in selectedSubmissionManifestFor(data)!.include"
                                :key="`include-${rule.kind}-${rule.path}`"
                              >
                                {{ manifestRuleLabel(rule) }}
                              </li>
                              <li v-if="selectedSubmissionManifestFor(data)!.include.length === 0">
                                无
                              </li>
                            </ul>
                          </div>
                          <div class="freeze-manifest-group">
                            <strong>必交文件</strong>
                            <ul>
                              <li
                                v-for="rule in selectedSubmissionManifestFor(data)!.required"
                                :key="`required-${rule.kind}-${rule.path}`"
                              >
                                {{ manifestRuleLabel(rule) }}
                              </li>
                              <li v-if="selectedSubmissionManifestFor(data)!.required.length === 0">
                                无
                              </li>
                            </ul>
                          </div>
                          <div class="freeze-manifest-group">
                            <strong>排除规则</strong>
                            <ul>
                              <li
                                v-for="rule in selectedSubmissionManifestFor(data)!.exclude"
                                :key="`exclude-${rule.kind}-${rule.path}`"
                              >
                                {{ manifestRuleLabel(rule) }}
                              </li>
                              <li v-if="selectedSubmissionManifestFor(data)!.exclude.length === 0">
                                无
                              </li>
                            </ul>
                          </div>
                          <div class="freeze-manifest-group">
                            <strong>允许 LLM 读取</strong>
                            <ul>
                              <li
                                v-for="rule in selectedSubmissionManifestFor(data)!.llmReadable"
                                :key="`llm-readable-${rule.kind}-${rule.path}`"
                              >
                                {{ manifestRuleLabel(rule) }}
                              </li>
                              <li v-if="selectedSubmissionManifestFor(data)!.llmReadable.length === 0">
                                无
                              </li>
                            </ul>
                          </div>
                          <p class="freeze-manifest-limits">
                            最多 {{ selectedSubmissionManifestFor(data)!.maxFiles }} 个文件，合计不超过
                            {{ selectedSubmissionManifestFor(data)!.maxTotalBytes }} 字节；
                            {{ selectedSubmissionManifestFor(data)!.followSymlinks ? '允许' : '不允许' }}跟随符号链接。
                          </p>
                        </div>
                        <details class="freeze-manifest-details">
                          <summary>查看完整清单 JSON</summary>
                          <pre class="freeze-manifest-json">{{ freezeManifestText(data) }}</pre>
                        </details>
                      </template>
                      <p class="freeze-confirm-hint">
                        提交前请确认清单覆盖全部必交文件。提交后平台会保存不可变快照，评测结果将在结果页出现。
                      </p>
                      <div class="env-failed-actions">
                        <button
                          type="button"
                          class="filled-button"
                          :disabled="!canFreeze(data) || !selectedSubmissionManifestFor(data) || freezeState.kind === 'loading'"
                          @click="confirmFreeze(data)"
                        >
                          确认冻结
                        </button>
                        <button
                          type="button"
                          class="text-button"
                          @click="freezeConfirmVisible = false"
                        >
                          取消
                        </button>
                      </div>
                    </div>

                    <div
                      v-if="freezeDiagnostic"
                      class="freeze-result"
                    >
                      <DiagnosticBanner
                        :code="freezeDiagnostic.code"
                        :request-id="freezeDiagnostic.requestId"
                        :next-step="freezeDiagnostic.nextStep"
                        :message="freezeDiagnostic.message"
                        :retryable="freezeDiagnostic.retryable"
                        severity="error"
                        @retry="retryFreeze"
                      />
                    </div>

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
                      <DiagnosticBanner
                        v-if="latestResultFailure"
                        :diagnostic="latestResultFailure"
                        severity="error"
                        data-testid="submission-result-failure"
                        @retry="latestResultState.load()"
                      />
                      <SubmissionProgress :stages="latestStages" />
                      <SubmissionTaskNotes :record="latestTask" />
                    </section>

                    <div
                      v-if="freezeEvidenceFor(data)"
                      class="evidence-card"
                    >
                      <div class="grant-row">
                        <span>提交 ID</span>
                        <code>{{ freezeEvidenceFor(data)?.submissionId }}</code>
                      </div>
                      <div class="grant-row">
                        <span>Object Version</span>
                        <code>{{ freezeEvidenceFor(data)?.object.objectVersion }}</code>
                      </div>
                      <div class="grant-row">
                        <span>SHA-256</span>
                        <code>{{ freezeEvidenceFor(data)?.contentSha256 }}</code>
                      </div>
                      <div class="grant-row">
                        <span>Media Type</span>
                        <code>{{ freezeEvidenceFor(data)?.object.mediaType }}</code>
                      </div>
                      <div class="grant-row">
                        <span>大小</span>
                        <code>{{ freezeEvidenceFor(data)?.object.sizeBytes }} B</code>
                      </div>
                      <p class="evidence-hint">
                        提交凭据将保留显示，可与「评测结果」页对账。
                      </p>
                    </div>
                  </div>
  </div>
</template>

<script setup lang="ts">
import { computed, watch } from 'vue'
import { RouterLink } from 'vue-router'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SubmissionProgress from '@/components/evaluation/SubmissionProgress.vue'
import SubmissionTaskNotes from '@/components/evaluation/SubmissionTaskNotes.vue'
import type { EnvironmentInstanceSchema } from '@/generated/contracts'
import { useEnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import { useSubmissionTasks } from '@/composables/useEvaluationTasks'
import { useEvaluationResult } from '@/composables/useEvaluationResults'
import { useSubmissionDirectory } from '@/composables/useSubmissionDirectory'
import { useVisiblePolling } from '@/composables/useVisiblePolling'
import { freezeStatusDescription, freezeStatusLabel, manifestRuleLabel } from '@/utils/environmentLifecycle'
import { evaluationOutcome } from '@/utils/evaluationOutcome'
import { isTerminalRun, readEvaluationTask, submissionStages } from '@/utils/evaluationProgress'
import { formatTimestamp } from '@/utils/format'

/** 实验提交与凭据：确认清单后冻结提交，显示提交凭据和最近一次提交的评测进度。 */
const props = defineProps<{ data: EnvironmentInstanceSchema }>()

const {
  freezeState,
  freezeDiagnostic,
  freezeConfirmVisible,
  freezeOperation,
  freezeStatus,
  selectedSubmissionManifestFor,
  freezeBlockReason,
  freezeManifestText,
  canFreeze,
  confirmFreeze,
  freezeEvidenceFor,
  retryFreeze,
  trackedSubmissionsVersion,
} = useEnvironmentEntryContext()

// Submission progress: the browser remembers only submission identifiers for
// the signed-in actor. The latest one is shown after the server returns it to
// this actor and names this environment as its source; every stage comes from
// the server.
const projectId = computed(() => props.data.projectId)
const submissionDirectory = useSubmissionDirectory(projectId)
watch(trackedSubmissionsVersion, () => submissionDirectory.reload())
watch(() => submissionDirectory.tracked, (ids) => { void submissionDirectory.resolve(ids) }, { immediate: true })
const latestSubmission = computed(() => {
  for (const submissionId of submissionDirectory.tracked) {
    const record = submissionDirectory.records[submissionId]
    if (record?.kind === 'ready' && record.environmentId === props.data.id) return { submissionId, frozenAt: record.frozenAt }
  }
  return null
})
const trackingProjectId = computed(() => latestSubmission.value ? props.data.projectId : undefined)
const submissionTasks = useSubmissionTasks(trackingProjectId)
const latestTask = computed(() => latestSubmission.value ? submissionTasks.records[latestSubmission.value.submissionId] ?? null : null)
const latestProgress = computed(() => latestTask.value?.fact.kind === 'progress' ? latestTask.value.fact.progress : null)
const latestRunId = computed(() => latestProgress.value?.runId)
const latestEnded = computed(() => Boolean(latestProgress.value && isTerminalRun(latestProgress.value)))
// The result is read once the server reports the run as ended.
const latestResultState = useEvaluationResult(trackingProjectId, computed(() => latestEnded.value ? latestRunId.value : undefined))
const latestResult = computed(() => latestEnded.value && latestResultState.result.kind === 'success' ? latestResultState.result.data : null)
// A result read that another read cannot fix (403, a non-retryable server
// answer, another run's data) ends the wait and is explained instead.
const latestResultFailure = computed(() => {
  const state = latestResultState.result
  return latestEnded.value && latestResultState.failureFinal && 'diagnostic' in state ? state.diagnostic : null
})
const latestSummary = computed(() => {
  if (latestResult.value) return evaluationOutcome(latestResult.value).summary
  const fact = latestTask.value?.fact
  if (latestResultFailure.value) return '评测已结束，但无法读取结果，页面已停止自动更新。'
  if (latestEnded.value) return '评测已结束，正在读取结果。页面会自动更新。'
  if (fact?.kind === 'progress') return `${readEvaluationTask(fact.progress).detail}离开后回到这里或“评测结果”页也能看到这次提交。`
  if (fact?.kind === 'not-created') return '提交已保存，平台还没有为它创建评测。页面会自动更新。'
  return '提交已保存，正在读取评测进度。页面会自动更新，离开后回到这里或“评测结果”页也能看到这次提交。'
})
const latestStages = computed(() => submissionStages({
  frozenAt: latestSubmission.value?.frozenAt ?? null,
  task: latestTask.value?.fact,
  result: latestResult.value,
  resultUnreadable: latestResultFailure.value !== null,
}, formatTimestamp))
const latestLink = computed(() => {
  const submissionId = latestSubmission.value?.submissionId
  return latestRunId.value
    ? { path: `/student/results/${latestRunId.value}`, query: { projectId: projectId.value, submissionId } }
    : { path: '/student/results', query: { projectId: projectId.value, submissionId } }
})
// Watch the project too, so the first read happens after the per-project
// stores have reset for the newly tracked project.
watch([() => latestSubmission.value?.submissionId, trackingProjectId], ([submissionId, project]) => {
  if (!submissionId || !project) return
  void submissionTasks.refresh([submissionId])
}, { immediate: true })
// Until the result is readable: progress while the run is open; after it
// ended, the result itself, which may become visible a little later (404) or
// fail once in a retryable way. A final failure stops the reads.
useVisiblePolling(
  () => {
    const submission = latestSubmission.value
    if (!submission) return undefined
    return latestEnded.value ? latestResultState.load({ quiet: true }) : submissionTasks.refresh([submission.submissionId])
  },
  computed(() => Boolean(latestSubmission.value) && !latestResult.value && !latestResultFailure.value),
)
</script>

<style scoped>
/* Layout wrapper only: its children stay in the parent's flex layout. */
.tab-body {
  display: contents;
}

.section-subtitle {
  font: var(--md-sys-title-small);
  color: var(--md-sys-color-on-surface);
  margin: var(--space-4) 0 var(--space-2);
}

.env-failed-actions {
  display: flex;
  gap: var(--space-2);
  margin-top: var(--space-2);
}

.evidence-hint {
  margin: var(--space-2) 0 0;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.freeze-status-card,
.console-freeze-pane,
.console-freeze-notice {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  padding: var(--space-4);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.freeze-status-card {
  margin-top: var(--space-4);
}

.freeze-status-header {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--space-3);
}

.freeze-status-label {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-medium);
}

.freeze-status-header strong {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-medium);
}

.freeze-status-card p,
.console-freeze-pane p,
.console-freeze-notice p {
  margin: 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
}

.freeze-status-diagnostic {
  color: var(--md-sys-color-error) !important;
}

.freeze-status-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  margin-top: var(--space-1);
}

.freeze-confirm {
  margin-top: var(--space-3);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  padding: var(--space-3) var(--space-4);
}

.freeze-manifest {
  margin: var(--space-2) 0;
  padding: var(--space-3);
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-small);
  overflow-x: auto;
}

.freeze-manifest-group + .freeze-manifest-group {
  margin-top: var(--space-3);
}

.freeze-manifest-group ul {
  margin: var(--space-1) 0 0;
  padding-left: var(--space-5);
}

.freeze-manifest-limits {
  margin: var(--space-3) 0 0;
  color: var(--md-sys-color-on-surface-variant);
}

.freeze-manifest-details {
  margin: var(--space-2) 0;
  font: var(--md-sys-body-small);
}

.freeze-manifest-details summary {
  cursor: pointer;
  color: var(--md-sys-color-primary);
}

.freeze-manifest-json {
  margin: var(--space-2) 0 0;
  padding: var(--space-3);
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container);
  font: 12px/1.5 monospace;
  overflow-x: auto;
  white-space: pre;
}

.freeze-unavailable {
  margin: var(--space-3) 0 0;
  color: var(--md-sys-color-error);
  font: var(--md-sys-body-small);
}

.freeze-confirm-hint {
  margin: 0 0 var(--space-2);
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.grant-row {
  display: flex;
  gap: var(--space-4);
  padding: var(--space-2) 0;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.grant-row:last-child {
  border-bottom: none;
}

.grant-row > span:first-child {
  width: 100px;
  flex-shrink: 0;
  color: var(--md-sys-color-on-surface-variant);
}

.freeze-section {
  margin-top: var(--space-6);
}

.freeze-desc {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  margin: 0 0 var(--space-3);
}

.freeze-result {
  margin-top: var(--space-4);
}

.evidence-card {
  margin-top: var(--space-4);
  padding: var(--space-4);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.submission-tracking {
  display: grid;
  gap: var(--space-3);
  margin-top: var(--space-4);
  padding: var(--space-4);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
}

.submission-tracking__head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-2) var(--space-4);
}

.submission-tracking__head h5 {
  margin: 0;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-small);
}

.submission-tracking__summary {
  margin: 0;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
}

.submission-tracking__link {
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
}
</style>
