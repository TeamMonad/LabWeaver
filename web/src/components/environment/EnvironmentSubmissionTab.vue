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
                      v-if="latestSubmission && latestSubmission.environmentId === data.id"
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
                          :to="{ path: '/student/results', query: { projectId: data.projectId, submissionId: latestSubmission.submissionId } }"
                        >
                          {{ latestResult ? '查看评测详情' : '在评测结果中查看' }}
                        </RouterLink>
                      </div>
                      <p
                        class="submission-tracking__summary"
                        role="status"
                      >
                        {{ latestResult ? evaluationOutcome(latestResult).summary : '提交已保存，正在等待评测结果。页面会自动更新，离开后回到这里或“评测结果”页也能看到这次提交。' }}
                      </p>
                      <SubmissionProgress :stages="latestStages" />
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
import { RouterLink } from 'vue-router'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SubmissionProgress from '@/components/evaluation/SubmissionProgress.vue'
import type { EnvironmentInstanceSchema } from '@/generated/contracts'
import { useEnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import { evaluationOutcome } from '@/utils/evaluationOutcome'
import { freezeStatusDescription, freezeStatusLabel, manifestRuleLabel } from '@/utils/environmentLifecycle'

/** 实验提交与凭据：确认清单后冻结提交，跟踪最近一次提交的评测进度。 */
defineProps<{ data: EnvironmentInstanceSchema }>()

const {
  freezeState,
  freezeDiagnostic,
  freezeConfirmVisible,
  latestSubmission,
  latestResult,
  latestStages,
  freezeOperation,
  freezeStatus,
  selectedSubmissionManifestFor,
  freezeBlockReason,
  freezeManifestText,
  canFreeze,
  confirmFreeze,
  freezeEvidenceFor,
  retryFreeze,
} = useEnvironmentEntryContext()
</script>

<style scoped>
/* Layout wrapper only: its children stay in the parent's flex layout. */
.tab-body {
  display: contents;
}

.submission-tracking {
  display: grid;
  gap: 12px;
  margin-top: 16px;
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
}

.submission-tracking__head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 8px 16px;
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

.section-subtitle {
  font: var(--md-sys-title-small);
  color: var(--md-sys-color-on-surface);
  margin: 16px 0 8px;
}

.env-failed-actions {
  display: flex;
  gap: 8px;
  margin-top: 8px;
}

.evidence-hint {
  margin: 8px 0 0;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.freeze-status-card,
.console-freeze-pane,
.console-freeze-notice {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.freeze-status-card {
  margin-top: 16px;
}

.freeze-status-header {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
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
  gap: 8px;
  margin-top: 4px;
}

.freeze-confirm {
  margin-top: 12px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  padding: 12px 16px;
}

.freeze-manifest {
  margin: 8px 0;
  padding: 12px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-small);
  overflow-x: auto;
}

.freeze-manifest-group + .freeze-manifest-group {
  margin-top: 12px;
}

.freeze-manifest-group ul {
  margin: 4px 0 0;
  padding-left: 20px;
}

.freeze-manifest-limits {
  margin: 12px 0 0;
  color: var(--md-sys-color-on-surface-variant);
}

.freeze-manifest-details {
  margin: 8px 0;
  font: var(--md-sys-body-small);
}

.freeze-manifest-details summary {
  cursor: pointer;
  color: var(--md-sys-color-primary);
}

.freeze-manifest-json {
  margin: 8px 0 0;
  padding: 12px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container);
  font: 12px/1.5 monospace;
  overflow-x: auto;
  white-space: pre;
}

.freeze-unavailable {
  margin: 12px 0 0;
  color: var(--md-sys-color-error);
  font: var(--md-sys-body-small);
}

.freeze-confirm-hint {
  margin: 0 0 8px;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.filled-button {
  height: 40px;
  padding: 0 24px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.filled-button.small {
  height: 32px;
  padding: 0 16px;
}

.filled-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.outlined-button {
  height: 40px;
  padding: 0 24px;
  border: 1px solid var(--md-sys-color-outline);
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.outlined-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.text-button {
  height: 40px;
  padding: 0 16px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.text-button.error {
  color: var(--md-sys-color-error);
}

.text-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.grant-row {
  display: flex;
  gap: 16px;
  padding: 8px 0;
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
  margin-top: 24px;
}

.freeze-desc {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  margin: 0 0 12px;
}

.freeze-result {
  margin-top: 16px;
}

.evidence-card {
  margin-top: 16px;
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}
</style>
