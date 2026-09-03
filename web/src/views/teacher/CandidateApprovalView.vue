<template>
  <div class="candidate-approval">
    <header class="page-header">
      <h2>候选审批与发布</h2>
      <p class="page-subtitle">审批候选后，以独立、可审计的操作发布 EnvironmentTemplateRelease 或 EvaluationRelease。</p>
    </header>

    <DiagnosticBanner
      v-if="!runId"
      code="RUN_ID_MISSING"
      message="缺少 runId 参数。请从材料上传页的 AgentRun 进入审批。"
      :retryable="false"
      severity="error"
    />

    <AsyncStateView v-else :state="approval.run" @retry="approval.load">
      <template #success="{ data: runData }">
        <div class="run-summary md-card">
          <div class="summary-row">
            <span class="summary-label">Run</span>
            <code class="summary-value">{{ runData.id }}</code>
          </div>
          <div class="summary-row">
            <span class="summary-label">状态</span>
            <GcpStatusPill :state="runData.state" domain="agent" />
          </div>
          <div class="summary-row">
            <span class="summary-label">请求 Runtime</span>
            <span class="summary-value">{{ runData.requestedRuntime }}</span>
          </div>
        </div>

        <section class="candidate-section" aria-labelledby="environment-heading">
          <h3 id="environment-heading" class="section-title">
            <SvgIcon name="environment" size="sm" aria-hidden="true" />
            Environment 候选
          </h3>

          <AsyncStateView :state="approval.environmentCandidate" @retry="approval.load">
            <template #success="{ data: view }">
              <div class="candidate-card md-card">
                <div class="candidate-meta">
                  <div class="meta-row">
                    <span class="meta-label">Candidate ID</span>
                    <code class="meta-value">{{ view.candidate.id }}</code>
                  </div>
                  <div class="meta-row">
                    <span class="meta-label">Revision</span>
                    <span class="meta-value">rev-{{ view.candidate.revision }}</span>
                  </div>
                  <div class="meta-row">
                    <span class="meta-label">Spec SHA-256</span>
                    <code class="meta-value">{{ truncateSha256(view.candidate.specSha256) }}</code>
                  </div>
                  <div class="meta-row">
                    <span class="meta-label">Policy Revision</span>
                    <span class="meta-value">rev-{{ view.candidate.policyRevision }}</span>
                  </div>
                  <div class="meta-row">
                    <span class="meta-label">Schema SHA-256</span>
                    <code class="meta-value">{{ truncateSha256(view.candidate.schemaSha256) }}</code>
                  </div>
                </div>

                <h4 class="subsection-title">候选 diff（与当前已发布版本对比）</h4>
                <StructuredDiff v-if="approval.environmentDiff.length" :changes="approval.environmentDiff" aria-label="Environment 候选与已发布版本差异" />
                <p v-else class="empty-note">无差异或已是最新版本。</p>

                <div class="approval-controls">
                  <textarea
                    v-model="environmentReason"
                    class="reason-input"
                    rows="2"
                    placeholder="审批理由（必填）"
                    aria-label="Environment 审批理由"
                  />
                  <div class="approval-buttons">
                    <button
                      type="button"
                      class="filled-button"
                      :disabled="!environmentReason.trim() || approval.deciding !== null"
                      @click="approval.decide('environment', 'approved', environmentReason)"
                    >
                      批准
                    </button>
                    <button
                      type="button"
                      class="outlined-button"
                      :disabled="!environmentReason.trim() || approval.deciding !== null"
                      @click="approval.decide('environment', 'rejected', environmentReason)"
                    >
                      拒绝
                    </button>
                    <button
                      type="button"
                      class="text-button"
                      :disabled="!environmentReason.trim() || approval.deciding !== null"
                      @click="approval.decide('environment', 'withdrawn', environmentReason)"
                    >
                      撤回
                    </button>
                  </div>
                </div>

                <div v-if="approval.latestEnvironmentApproval" class="approval-status" :class="`approval-status--${approval.latestEnvironmentApproval.decision}`">
                  最新审批：{{ approval.latestEnvironmentApproval.decision }} — {{ approval.latestEnvironmentApproval.reason }}
                </div>

                <div v-if="approval.decisionDiagnostic?.kind === 'environment'" class="approval-decision-error">
                  <DiagnosticBanner
                    :code="approval.decisionDiagnostic.diagnostic.code"
                    :message="approval.decisionDiagnostic.diagnostic.message"
                    :retryable="approval.decisionDiagnostic.diagnostic.retryable"
                    severity="warning"
                  />
                </div>
              </div>

              <section class="release-section" aria-labelledby="release-heading">
                <h4 id="release-heading" class="section-title">
                  <SvgIcon name="publish" size="sm" aria-hidden="true" />
                  发布证据
                </h4>

                <div class="evidence-card md-card">
                  <div class="evidence-row">
                    <span class="evidence-label">Digest</span>
                    <code class="evidence-value">{{ view.build?.artifact?.kind === 'container' ? view.build.artifact.digest : '—' }}</code>
                  </div>
                  <div class="evidence-row">
                    <span class="evidence-label">Repository</span>
                    <code class="evidence-value">{{ view.build?.artifact?.kind === 'container' ? view.build.artifact.repository : '—' }}</code>
                  </div>
                  <div class="evidence-row">
                    <span class="evidence-label">Trivy</span>
                    <span class="evidence-value">
                      Critical {{ view.build?.imagePolicyEvaluation?.vulnerabilities.critical ?? 0 }},
                      High {{ view.build?.imagePolicyEvaluation?.vulnerabilities.high ?? 0 }},
                      Medium {{ view.build?.imagePolicyEvaluation?.vulnerabilities.medium ?? 0 }},
                      Low {{ view.build?.imagePolicyEvaluation?.vulnerabilities.low ?? 0 }}
                    </span>
                  </div>
                </div>

                <DiagnosticBanner
                  v-if="approval.imageGate.status === 'blocked'"
                  :code="approval.imageGate.reasons[0] ?? 'RELEASE_GATE_BLOCKED'"
                  :message="approval.imageGate.reasons.join('；')"
                  :retryable="false"
                  severity="error"
                />
                <DiagnosticBanner
                  v-else-if="approval.imageGate.status === 'warning'"
                  :code="approval.imageGate.reasons[0] ?? 'RELEASE_GATE_WARNING'"
                  :message="approval.imageGate.reasons.join('；')"
                  :retryable="false"
                  severity="warning"
                />

                <div class="publish-actions">
                  <button
                    type="button"
                    class="filled-button"
                    :disabled="!approval.canPublish"
                    @click="publishConfirmOpen = true"
                  >
                    发布 EnvironmentTemplateRelease
                  </button>
                  <p v-if="!approval.canPublish" class="publish-hint">
                    需要 Environment 候选已批准且镜像证据通过门禁。
                  </p>
                </div>

                <AsyncStateView v-if="approval.publish.kind !== 'idle'" :state="approval.publish">
                  <template #success="{ data }">
                    <div class="publish-success">
                      <SvgIcon name="check_circle" size="md" aria-hidden="true" />
                      <span>已接受发布请求：{{ data.operationId }}</span>
                    </div>
                  </template>
                </AsyncStateView>
              </section>
            </template>
          </AsyncStateView>
        </section>

        <section class="candidate-section" aria-labelledby="evaluation-heading">
          <h3 id="evaluation-heading" class="section-title">
            <SvgIcon name="grading" size="sm" aria-hidden="true" />
            Evaluation 候选
          </h3>

          <p class="sprint-note">审批不会自动发布；Evaluation Runtime 身份由 Control 从受控配置构造，浏览器只读展示。</p>

          <AsyncStateView :state="approval.evaluationCandidate" @retry="approval.load">
            <template #success="{ data: view }">
              <div class="candidate-card md-card">
                <div class="candidate-meta">
                  <div class="meta-row">
                    <span class="meta-label">Candidate ID</span>
                    <code class="meta-value">{{ view.candidate.id }}</code>
                  </div>
                  <div class="meta-row">
                    <span class="meta-label">Revision</span>
                    <span class="meta-value">rev-{{ view.candidate.revision }}</span>
                  </div>
                  <div class="meta-row">
                    <span class="meta-label">Spec SHA-256</span>
                    <code class="meta-value">{{ truncateSha256(view.candidate.specSha256) }}</code>
                  </div>
                  <div class="meta-row">
                    <span class="meta-label">Policy Revision</span>
                    <span class="meta-value">rev-{{ view.candidate.policyRevision }}</span>
                  </div>
                  <div class="meta-row">
                    <span class="meta-label">Schema SHA-256</span>
                    <code class="meta-value">{{ truncateSha256(view.candidate.schemaSha256) }}</code>
                  </div>
                </div>

                <h4 class="subsection-title">候选 diff</h4>
                <StructuredDiff v-if="approval.evaluationDiff.length" :changes="approval.evaluationDiff" aria-label="Evaluation 候选差异" />
                <p v-else class="empty-note">无差异。</p>

                <div class="approval-controls">
                  <textarea
                    v-model="evaluationReason"
                    class="reason-input"
                    rows="2"
                    placeholder="审批理由（必填）"
                    aria-label="Evaluation 审批理由"
                  />
                  <div class="approval-buttons">
                    <button
                      type="button"
                      class="filled-button"
                      :disabled="!evaluationReason.trim() || approval.deciding !== null"
                      @click="approval.decide('evaluation', 'approved', evaluationReason)"
                    >
                      批准
                    </button>
                    <button
                      type="button"
                      class="outlined-button"
                      :disabled="!evaluationReason.trim() || approval.deciding !== null"
                      @click="approval.decide('evaluation', 'rejected', evaluationReason)"
                    >
                      拒绝
                    </button>
                    <button
                      type="button"
                      class="text-button"
                      :disabled="!evaluationReason.trim() || approval.deciding !== null"
                      @click="approval.decide('evaluation', 'withdrawn', evaluationReason)"
                    >
                      撤回
                    </button>
                  </div>
                </div>

                <div v-if="approval.latestEvaluationApproval" class="approval-status" :class="`approval-status--${approval.latestEvaluationApproval.decision}`">
                  最新审批：{{ approval.latestEvaluationApproval.decision }} — {{ approval.latestEvaluationApproval.reason }}
                </div>

                <div v-if="approval.decisionDiagnostic?.kind === 'evaluation'" class="approval-decision-error">
                  <DiagnosticBanner
                    :code="approval.decisionDiagnostic.diagnostic.code"
                    :message="approval.decisionDiagnostic.diagnostic.message"
                    :retryable="approval.decisionDiagnostic.diagnostic.retryable"
                    severity="warning"
                  />
                </div>
              </div>

              <section class="release-section" aria-labelledby="evaluation-release-heading">
                <h4 id="evaluation-release-heading" class="section-title">
                  <SvgIcon name="publish" size="sm" aria-hidden="true" />
                  EvaluationRelease
                </h4>
                <div class="publish-actions">
                  <button
                    type="button"
                    class="filled-button"
                    :disabled="!evaluationReleases.canPublish"
                    @click="evaluationPublishConfirmOpen = true"
                  >
                    发布 EvaluationRelease
                  </button>
                  <p v-if="!evaluationReleases.canPublish" class="publish-hint">
                    需要当前 Evaluation 候选已批准，且尚未发布同一候选 revision。
                  </p>
                </div>

                <AsyncStateView v-if="evaluationReleases.publication.kind !== 'idle'" :state="evaluationReleases.publication">
                  <template #success="{ data }">
                    <div class="publish-success">
                      <SvgIcon name="check_circle" size="md" aria-hidden="true" />
                      <span>EvaluationRelease 已发布：{{ data.id }}</span>
                    </div>
                  </template>
                </AsyncStateView>

                <AsyncStateView
                  class="release-list-state"
                  :state="evaluationReleases.releases"
                  empty-text="尚未发布 EvaluationRelease"
                  @retry="evaluationReleases.load"
                >
                  <template #success="{ data: releases }">
                    <ul class="release-list" aria-label="EvaluationRelease 列表">
                      <li v-for="release in releases" :key="release.id" class="release-card md-card">
                        <button type="button" class="release-select" @click="evaluationReleases.select(release.id)">
                          <span><code>{{ release.id }}</code></span>
                          <span>rev-{{ release.revision }} · {{ release.state }}</span>
                        </button>
                        <button
                          v-if="release.state === 'active'"
                          type="button"
                          class="outlined-button"
                          :disabled="evaluationReleases.withdrawingId !== null"
                          @click="evaluationWithdrawTarget = release"
                        >
                          撤回
                        </button>
                      </li>
                    </ul>
                  </template>
                </AsyncStateView>

                <AsyncStateView v-if="evaluationReleases.selected.kind !== 'idle'" :state="evaluationReleases.selected" @retry="reloadSelectedRelease">
                  <template #success="{ data: release }">
                    <article class="runtime-card md-card" aria-labelledby="runtime-identity-heading">
                      <h5 id="runtime-identity-heading" class="subsection-title">受控 Runtime 身份（只读）</h5>
                      <dl class="runtime-grid">
                        <template v-for="entry in runtimeIdentityEntries(release)" :key="entry.label">
                          <dt>{{ entry.label }}</dt>
                          <dd><code>{{ entry.value }}</code></dd>
                        </template>
                      </dl>
                      <DiagnosticBanner
                        v-if="release.withdrawalDiagnosticCode"
                        :code="release.withdrawalDiagnosticCode"
                        message="该 Release 已撤回；历史终态结果仍保留。"
                        :retryable="false"
                        severity="warning"
                      />
                    </article>
                  </template>
                </AsyncStateView>
              </section>
            </template>
          </AsyncStateView>
        </section>
      </template>
    </AsyncStateView>

    <ConfirmDialog
      :open="publishConfirmOpen"
      title="确认发布 EnvironmentTemplateRelease"
      :description="`将绑定候选 rev-${approval.environmentCandidate.kind === 'success' ? approval.environmentCandidate.data.candidate.revision : '?'} 与镜像 digest。发布后不可变，是否继续？`"
      confirm-text="发布"
      severity="warning"
      @confirm="onPublishConfirmed"
      @cancel="publishConfirmOpen = false"
    />
    <ConfirmDialog
      :open="evaluationPublishConfirmOpen"
      title="确认发布 EvaluationRelease"
      :description="`将发布当前已批准的 Evaluation 候选 rev-${evaluationCandidateView?.candidate.revision ?? '?'}。Runtime 身份由 Control 固定构造，是否继续？`"
      confirm-text="发布"
      severity="warning"
      @confirm="onEvaluationPublishConfirmed"
      @cancel="evaluationPublishConfirmOpen = false"
    />
    <ConfirmDialog
      :open="evaluationWithdrawTarget !== null"
      title="确认撤回 EvaluationRelease"
      :description="`撤回 release rev-${evaluationWithdrawTarget?.revision ?? '?'} 后不会删除历史终态结果，是否继续？`"
      confirm-text="撤回"
      severity="warning"
      @confirm="onEvaluationWithdrawConfirmed"
      @cancel="evaluationWithdrawTarget = null"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { useRoute } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import StructuredDiff from '@/components/common/StructuredDiff.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import GcpStatusPill from '@/components/common/GcpStatusPill.vue'
import { useCourseContext } from '@/composables/useCourseContext'
import { useCandidateApproval } from '@/composables/useCandidateApproval'
import { useEvaluationReleases } from '@/composables/useEvaluationReleases'
import type { EvaluationReleaseSchema } from '@/generated/contracts'
import { truncateSha256 } from '@/utils/format'

const route = useRoute()
const runId = computed(() => (typeof route.query.runId === 'string' ? route.query.runId : undefined))

const course = useCourseContext()
const approval = useCandidateApproval(course.courseId, runId)
const evaluationCandidateView = computed(() => approval.evaluationCandidate.kind === 'success' ? approval.evaluationCandidate.data : null)
const evaluationApproval = computed(() => approval.latestEvaluationApproval ?? null)
const evaluationReleases = useEvaluationReleases(course.courseId, evaluationCandidateView, evaluationApproval)

const environmentReason = ref('')
const evaluationReason = ref('')
const publishConfirmOpen = ref(false)
const evaluationPublishConfirmOpen = ref(false)
const evaluationWithdrawTarget = ref<EvaluationReleaseSchema | null>(null)

function onPublishConfirmed() {
  publishConfirmOpen.value = false
  approval.publishRelease()
}

async function onEvaluationPublishConfirmed() {
  evaluationPublishConfirmOpen.value = false
  await evaluationReleases.publish()
}

async function onEvaluationWithdrawConfirmed() {
  const target = evaluationWithdrawTarget.value
  evaluationWithdrawTarget.value = null
  if (target) await evaluationReleases.withdraw(target)
}

function reloadSelectedRelease() {
  if (evaluationReleases.selected.kind === 'success') evaluationReleases.select(evaluationReleases.selected.data.id)
}

function runtimeIdentityEntries(release: EvaluationReleaseSchema) {
  const identity = release.runtimeIdentity
  return [
    { label: 'Provider binding', value: identity.providerBinding },
    { label: 'Runner image', value: identity.runnerImage },
    { label: 'Package SHA-256', value: identity.packageSha256 },
    { label: 'Configuration SHA-256', value: identity.configurationSha256 },
    { label: 'Migration catalog SHA-256', value: identity.migrationCatalogSha256 },
    { label: 'Runtime artifact SHA-256', value: identity.runtimeArtifactSha256 },
    { label: 'Source SHA-256', value: identity.sourceSha256 },
  ]
}
</script>

<style scoped>
.candidate-approval {
  display: flex;
  flex-direction: column;
  gap: 28px;
}

.page-header h2 {
  font: var(--md-sys-headline-small);
  color: var(--md-sys-color-on-surface);
  margin: 0;
}

.page-subtitle {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
  margin: 4px 0 0;
}

.section-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font: var(--md-sys-title-medium);
  color: var(--md-sys-color-on-surface);
  margin: 0 0 12px;
}

.subsection-title {
  font: var(--md-sys-title-small);
  color: var(--md-sys-color-on-surface);
  margin: 20px 0 8px;
}

.run-summary,
.candidate-card,
.evidence-card {
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.summary-row,
.meta-row,
.evidence-row {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 8px 0;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.summary-row:last-child,
.meta-row:last-child,
.evidence-row:last-child {
  border-bottom: none;
}

.summary-label,
.meta-label,
.evidence-label {
  width: 140px;
  flex-shrink: 0;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.summary-value,
.meta-value,
.evidence-value {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface);
  word-break: break-all;
}

.approval-controls {
  margin-top: 20px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.reason-input {
  width: 100%;
  padding: 12px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
  resize: vertical;
}

.approval-buttons {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
}

.filled-button,
.outlined-button,
.text-button {
  height: 40px;
  padding: 0 24px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.filled-button {
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
}

.filled-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.outlined-button {
  border: 1px solid var(--md-sys-color-outline);
  background: transparent;
  color: var(--md-sys-color-primary);
}

.outlined-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.text-button {
  background: transparent;
  color: var(--md-sys-color-primary);
}

.text-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.approval-status {
  margin-top: 16px;
  padding: 10px 12px;
  border-radius: var(--md-sys-shape-medium);
  font: var(--md-sys-body-medium);
}

.approval-status--approved {
  background: var(--md-sys-color-tertiary-container);
  color: var(--md-sys-color-on-tertiary-container);
}

.approval-status--rejected {
  background: var(--md-sys-color-error-container);
  color: var(--md-sys-color-on-error-container);
}

.approval-status--withdrawn {
  background: var(--md-sys-color-surface-container-highest);
  color: var(--md-sys-color-on-surface-variant);
}

.release-section {
  margin-top: 24px;
}

.publish-actions {
  margin-top: 16px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.publish-hint {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  margin: 0;
}

.publish-success {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 16px;
  padding: 10px 12px;
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-tertiary-container);
  color: var(--md-sys-color-on-tertiary-container);
  font: var(--md-sys-body-medium);
}

.sprint-note {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  margin: 0 0 12px;
}

.empty-note {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  margin: 0;
}

.release-list-state {
  margin-top: 16px;
}

.release-list {
  display: grid;
  gap: 10px;
  list-style: none;
  margin: 0;
  padding: 0;
}

.release-card {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px;
}

.release-select {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
  padding: 4px;
  border: 0;
  background: transparent;
  color: var(--md-sys-color-on-surface);
  text-align: left;
  cursor: pointer;
}

.release-select code {
  overflow-wrap: anywhere;
}

.runtime-card {
  margin-top: 16px;
  padding: 16px;
}

.runtime-grid {
  display: grid;
  grid-template-columns: minmax(150px, 200px) 1fr;
  gap: 8px 16px;
  margin: 0;
}

.runtime-grid dt {
  color: var(--md-sys-color-on-surface-variant);
}

.runtime-grid dd {
  min-width: 0;
  margin: 0;
  overflow-wrap: anywhere;
}

@media (max-width: 600px) {
  .summary-row,
  .meta-row,
  .evidence-row,
  .release-card {
    align-items: stretch;
    flex-direction: column;
  }

  .summary-label,
  .meta-label,
  .evidence-label {
    width: auto;
  }

  .runtime-grid {
    grid-template-columns: 1fr;
  }
}
</style>
