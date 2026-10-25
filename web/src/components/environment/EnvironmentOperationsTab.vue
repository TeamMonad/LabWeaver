<template>
  <div class="tab-body">
                  <div
                    v-if="activeOperation"
                    class="operation-current-panel md-card"
                    role="status"
                  >
                    <div class="operation-current-header">
                      <div>
                        <span class="operation-current-label">当前操作</span>
                        <strong>{{ environmentOperationKindLabel(activeOperation.kind) }}</strong>
                      </div>
                      <span
                        class="env-state"
                        :class="`env-state--${activeOperation.state}`"
                      >
                        {{ environmentOperationStateLabel(activeOperation.state) }}
                      </span>
                    </div>
                    <p class="operation-current-description">
                      {{ operationStatusDescription(activeOperation.state) }}
                      第 {{ activeOperation.attempt }}/{{ activeOperation.maxAttempts }} 次尝试。
                    </p>
                    <dl class="operation-current-meta">
                      <div>
                        <dt>受理时间</dt>
                        <dd>{{ formatTimestamp(activeOperation.acceptedAt) }}</dd>
                      </div>
                      <div>
                        <dt>处理截止</dt>
                        <dd>{{ formatTimestamp(activeOperation.deadlineAt) }}</dd>
                      </div>
                      <div v-if="activeOperation.cleanupStartedAt">
                        <dt>资源清理开始</dt>
                        <dd>{{ formatTimestamp(activeOperation.cleanupStartedAt) }}</dd>
                      </div>
                    </dl>
                    <div
                      v-if="activeOperation.cancelEligible"
                      class="env-failed-actions"
                    >
                      <button
                        type="button"
                        class="outlined-button"
                        :disabled="operations.cancelling || !canCancelOperation"
                        :title="permissions.denialReason('stop', '取消操作') ?? undefined"
                        @click="cancelCurrentOperation"
                      >
                        {{ operations.cancelling ? '取消中…' : '取消操作' }}
                      </button>
                      <p
                        v-if="!canCancelOperation"
                        class="operation-permission-hint"
                        role="note"
                      >
                        {{ permissions.denialReason('stop', '取消操作') }}
                      </p>
                    </div>
                  </div>

                  <div
                    v-if="operations.cancelDiagnostic"
                    class="lifecycle-result"
                  >
                    <DiagnosticBanner
                      :code="operations.cancelDiagnostic.code"
                      :request-id="operations.cancelDiagnostic.requestId"
                      :next-step="operations.cancelDiagnostic.nextStep"
                      :message="operations.cancelDiagnostic.message"
                      :retryable="operations.cancelDiagnostic.retryable"
                      severity="error"
                      @retry="cancelCurrentOperation"
                    />
                  </div>

                  <div
                    v-if="data.observedState === 'failed' || failedOperation"
                    class="env-failed-panel"
                  >
                    <DiagnosticBanner
                      :message="failedEnvironmentMessage(data)"
                      :retryable="false"
                      severity="error"
                    />
                  </div>

                  <div
                    v-if="retryDiagnostic"
                    class="lifecycle-result"
                  >
                    <DiagnosticBanner
                      :code="retryDiagnostic.code"
                      :request-id="retryDiagnostic.requestId"
                      :next-step="retryDiagnostic.nextStep"
                      :message="retryDiagnostic.message"
                      :retryable="false"
                      severity="error"
                    />
                    <RouterLink
                      v-if="requiresNewResourceRequest(retryDiagnostic.code) && data.class === 'work'"
                      class="outlined-button small"
                      data-testid="retry-lease-new-request-link"
                      :to="{ path: '/researcher/resources', query: { projectId: data.projectId, releaseId: data.releaseId, releaseVersion: String(data.releaseVersion) } }"
                    >
                      提交新的资源申请
                    </RouterLink>
                  </div>

                  <AsyncStateView
                    :state="operations.operations"
                    @retry="operations.load"
                  >
                    <template #success>
                      <div
                        v-if="operationTimeline.length > 0"
                        class="timeline-section"
                      >
                        <h4 class="section-subtitle">
                          操作与诊断时间线
                        </h4>
                        <EventTimeline
                          :events="operationTimeline"
                          aria-label="环境操作与诊断时间线"
                        />
                      </div>
                    </template>
                  </AsyncStateView>
  </div>
</template>

<script setup lang="ts">
import { RouterLink } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import EventTimeline from '@/components/common/EventTimeline.vue'
import type { EnvironmentInstanceSchema } from '@/generated/contracts'
import { useEnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import { formatTimestamp } from '@/utils/format'
import { environmentOperationKindLabel, environmentOperationStateLabel } from '@/utils/stateLabels'
import { operationStatusDescription } from '@/utils/environmentLifecycle'
import { requiresNewResourceRequest } from '@/types/async'

/** 异步操作与诊断：当前操作、取消、失败说明和操作时间线。 */
defineProps<{ data: EnvironmentInstanceSchema }>()

const {
  operations,
  operationTimeline,
  activeOperation,
  failedOperation,
  cancelCurrentOperation,
  canCancelOperation,
  permissions,
  retryDiagnostic,
  failedEnvironmentMessage,
} = useEnvironmentEntryContext()
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

.lifecycle-result {
  margin-top: var(--space-4);
}

.env-state {
  padding: var(--space-1) var(--space-3);
  border-radius: var(--md-sys-shape-small);
  font: var(--md-sys-label-large);
  text-transform: capitalize;
}

.env-state--requested,
.env-state--validating,
.env-state--building,
.env-state--provisioning,
.env-state--stopping,
.env-state--updating,
.env-state--expiring,
.env-state--deleting,
.env-state--accepted,
.env-state--running,
.env-state--cancelling {
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
}

.env-state--ready,
.env-state--succeeded {
  background: var(--md-sys-color-tertiary-container);
  color: var(--md-sys-color-on-tertiary-container);
}

.env-state--stopped,
.env-state--cancelled {
  background: var(--md-sys-color-surface-container-highest);
  color: var(--md-sys-color-on-surface-variant);
}

.env-state--failed,
.env-state--deleted {
  background: var(--md-sys-color-error-container);
  color: var(--md-sys-color-on-error-container);
}

.operation-current-panel {
  margin-bottom: var(--space-4);
}

.operation-current-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--space-4);
}

.operation-current-header > div {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
}

.operation-current-label {
  font: var(--md-sys-label-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.operation-current-header strong {
  font: var(--md-sys-title-medium);
  color: var(--md-sys-color-on-surface);
}

.operation-current-description {
  margin: var(--space-4) 0;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.operation-current-meta {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: var(--space-3) var(--space-4);
  margin: 0;
}

.operation-current-meta div {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
}

.operation-current-meta dt {
  font: var(--md-sys-label-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.operation-current-meta dd {
  margin: 0;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface);
}

.env-failed-panel {
  margin-bottom: var(--space-4);
}

.env-failed-actions {
  display: flex;
  gap: var(--space-2);
  margin-top: var(--space-2);
}

.operation-permission-hint {
  margin: 0;
  align-self: center;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.timeline-section {
  margin-top: var(--space-6);
}
</style>
