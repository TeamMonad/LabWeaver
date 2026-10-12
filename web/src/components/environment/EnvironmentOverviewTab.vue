<template>
  <div class="tab-body">
                  <div class="env-card md-card">
                    <div class="env-meta-grid">
                      <div class="meta-item">
                        <span class="meta-item__label">运行方式</span>
                        <span class="meta-item__value">{{ data.runtimeKind === 'container' ? '容器' : '虚拟机' }}</span>
                      </div>
                      <div class="meta-item">
                        <span class="meta-item__label">期望状态</span>
                        <span class="meta-item__value">{{ desiredEnvironmentStateLabel(data.desiredState) }}</span>
                      </div>
                      <div class="meta-item">
                        <span class="meta-item__label">材料保留至</span>
                        <span class="meta-item__value">{{ environmentMaterialDeadlineLabel(data.eligibilityExpiresAt) }}</span>
                      </div>
                    </div>
                    <details class="environment-id-details">
                      <summary>查看高级详情</summary>
                      <span>环境 ID</span>
                      <code>{{ data.id }}</code>
                      <span>内部修订版本：rev-{{ data.revision }}</span>
                    </details>
                  </div>

                  <div class="access-section">
                    <h4 class="section-subtitle">
                      访问端点与授权
                    </h4>
                    <AsyncStateView
                      :state="access.endpoints"
                      @retry="access.loadEndpoints"
                    >
                      <template #success="{ data: eps }">
                        <DataTable
                          :columns="endpointColumns"
                          :rows="eps"
                          aria-label="环境入口"
                        >
                          <template #protocol="{ row }">
                            <span class="tag">{{ row.protocol }}</span>
                          </template>
                          <template #health="{ row }">
                            <span
                              class="health-dot"
                              :class="`health-dot--${row.health}`"
                            />
                            {{ endpointHealthLabel(row.health) }}
                          </template>
                          <template #observedAt="{ row }">
                            {{ formatTimestamp(row.observedAt) }}
                          </template>
                        </DataTable>

                        <div
                          v-if="access.grant.kind === 'success' || canIssueAccessGrant(data)"
                          class="grant-actions"
                        >
                          <button
                            v-if="access.grant.kind !== 'success' && canIssueAccessGrant(data)"
                            type="button"
                            class="filled-button"
                            :disabled="access.creating || eps.length === 0"
                            @click="issueAccessGrant"
                          >
                            签发访问授权
                          </button>
                          <button
                            v-if="access.grant.kind === 'success'"
                            type="button"
                            class="text-button error"
                            @click="revokeAccessGrant"
                          >
                            撤销授权
                          </button>
                        </div>

                        <p
                          v-if="access.grant.kind !== 'success' && !canIssueAccessGrant(data)"
                          class="access-grant-hint"
                          role="status"
                        >
                          {{ accessGrantBlockReason(data) }}
                        </p>

                        <div
                          v-if="createGrantDiagnostic"
                          class="grant-result"
                        >
                          <DiagnosticBanner
                            :code="createGrantDiagnostic.code"
                            :request-id="createGrantDiagnostic.requestId"
                            :next-step="createGrantDiagnostic.nextStep"
                            :message="createGrantDiagnostic.message"
                            :retryable="createGrantDiagnostic.retryable"
                            severity="error"
                            @retry="issueAccessGrant"
                          />
                        </div>

                        <AsyncStateView
                          :state="access.grant"
                          @retry="issueAccessGrant"
                        >
                          <template #success="{ data: g }">
                            <div class="grant-card">
                              <div class="grant-row">
                                <span>授权 ID</span>
                                <code>{{ g.id }}</code>
                              </div>
                              <div class="grant-row">
                                <span>状态</span>
                                <span
                                  class="env-state"
                                  :class="`env-state--${g.state}`"
                                >{{ accessGrantStateLabel(g.state) }}</span>
                              </div>
                              <div class="grant-row">
                                <span>有效期</span>
                                <span>{{ formatTimestamp(g.issuedAt) }} → {{ formatTimestamp(g.expiresAt) }}</span>
                              </div>
                              <div class="grant-row">
                                <span>过期</span>
                                <span>{{ formatExpiry(g.expiresAt) }}</span>
                              </div>
                              <div class="grant-row">
                                <span>入口授权</span>
                                <div class="endpoint-grants">
                                  <span
                                    v-for="eg in g.endpointGrants"
                                    :key="eg.id"
                                    class="tag"
                                  >
                                    {{ eg.protocol }} {{ eg.alias ?? eg.endpointId }}
                                  </span>
                                </div>
                              </div>
                            </div>

                            <div
                              v-if="g.state === 'active'"
                              class="runtime-access"
                            >
                              <div
                                v-if="webGrant(g)"
                                class="access-card"
                              >
                                <h5 class="access-card__title">
                                  <SvgIcon
                                    name="code"
                                    size="sm"
                                    aria-hidden="true"
                                  />
                                  容器实验入口
                                </h5>
                                <p class="access-card__desc">
                                  通过当前登录会话与访问授权打开受保护的容器实验页面。
                                </p>
                                <button
                                  type="button"
                                  class="filled-button"
                                  :disabled="!connectUrl(g)"
                                  @click="openContainerRuntime(g)"
                                >
                                  打开容器实验
                                </button>
                                <p
                                  v-if="!connectUrl(g)"
                                  class="access-card__hint"
                                >
                                  连接地址缺失，无法打开。
                                </p>
                              </div>

                              <div
                                v-if="sshGrant(g)"
                                class="access-card"
                              >
                                <h5 class="access-card__title">
                                  <SvgIcon
                                    name="terminal"
                                    size="sm"
                                    aria-hidden="true"
                                  />
                                  SSH
                                </h5>
                                <p class="access-card__desc">
                                  单行命令连接到唯一虚拟机；无需下载配置。
                                </p>
                                <div
                                  v-if="sshCommand(g)"
                                  class="ssh-command"
                                >
                                  <code class="ssh-command__text">{{ sshCommand(g) }}</code>
                                  <CopyButton
                                    :text="sshCommand(g) ?? ''"
                                    aria-label="复制 SSH 命令"
                                  />
                                </div>
                                <div
                                  v-if="sshCommand(g)"
                                  class="ssh-meta"
                                >
                                  <span>网关指纹：<code>{{ sshFingerprint(g) ?? '不可用' }}</code></span>
                                  <span>访问授权有效期：{{ formatExpiry(g.expiresAt) }}</span>
                                </div>
                                <p
                                  v-else
                                  class="access-card__hint"
                                >
                                  SSH 别名或 Gateway 缺失，无法生成命令。
                                </p>
                              </div>
                            </div>
                          </template>
                        </AsyncStateView>
                      </template>
                    </AsyncStateView>
                  </div>
  </div>
</template>

<script setup lang="ts">
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DataTable from '@/components/common/DataTable.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import CopyButton from '@/components/common/CopyButton.vue'
import type { DataTableColumn } from '@/components/common/DataTable.vue'
import type { EnvironmentInstanceSchema } from '@/generated/contracts'
import { useEnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import { formatTimestamp } from '@/utils/format'
import { accessGrantStateLabel, endpointHealthLabel } from '@/utils/stateLabels'
import { formatExpiry } from '@/types/access'
import { accessGrantBlockReason, canIssueAccessGrant, desiredEnvironmentStateLabel, environmentMaterialDeadlineLabel } from '@/utils/environmentLifecycle'
import { connectUrl, openContainerRuntime, sshCommand, sshFingerprint, sshGrant, webGrant } from '@/utils/environmentAccessGrant'

/** 概览与访问：环境信息、访问端点、访问授权、容器入口和 SSH。 */
defineProps<{ data: EnvironmentInstanceSchema }>()

const {
  createGrantDiagnostic,
  access,
  issueAccessGrant,
  revokeAccessGrant,
} = useEnvironmentEntryContext()

const endpointColumns: DataTableColumn<{ protocol: string; health: string; observedAt: string; id: string }>[] = [
  { key: 'protocol', title: '协议' },
  { key: 'health', title: '健康' },
  { key: 'observedAt', title: '观测时间' },
]
</script>

<style scoped>
/* Layout wrapper only: its children stay in the parent's flex layout. */
.tab-body {
  display: contents;
}

.env-meta-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: 16px;
  padding: 16px;
}

.meta-item {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.meta-item__label {
  font: var(--md-sys-label-small);
  color: var(--md-sys-color-on-surface-variant);
}

.meta-item__value {
  font: var(--md-sys-body-medium);
  font-weight: 500;
  color: var(--md-sys-color-on-surface);
}

.environment-id-details,
.environment-id-input-details {
  margin: 0 16px 16px;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-small);
}

.environment-id-details summary,
.environment-id-input-details summary {
  cursor: pointer;
}

.environment-id-details code {
  display: block;
  margin-top: 6px;
  overflow-wrap: anywhere;
  color: var(--md-sys-color-on-surface);
}

.environment-id-details span {
  display: block;
  margin-top: 6px;
}

.section-subtitle {
  font: var(--md-sys-title-small);
  color: var(--md-sys-color-on-surface);
  margin: 16px 0 8px;
}

.env-card,
.grant-card {
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.env-state {
  padding: 4px 12px;
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

.access-section {
  margin-top: 24px;
}

.grant-actions {
  display: flex;
  gap: 12px;
  margin-top: 16px;
}

.access-grant-hint {
  margin: 12px 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.grant-result {
  margin-top: 16px;
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

.endpoint-grants {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.tag {
  padding: 4px 10px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
  font: var(--md-sys-label-medium);
}

.health-dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  margin-right: 6px;
  border-radius: 50%;
  background: var(--md-sys-color-outline);
}

.health-dot--healthy { background: var(--md-sys-color-tertiary); }

.health-dot--unhealthy { background: var(--md-sys-color-error); }

.health-dot--pending { background: var(--md-sys-color-primary); }

.health-dot--removed { background: var(--md-sys-color-outline); }

.runtime-access {
  display: flex;
  flex-direction: column;
  gap: 16px;
  margin-top: 20px;
}

.access-card {
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.access-card__title {
  display: flex;
  align-items: center;
  gap: 8px;
  font: var(--md-sys-title-small);
  color: var(--md-sys-color-on-surface);
  margin: 0 0 8px;
}

.access-card__desc {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  margin: 0 0 12px;
}

.access-card__hint {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-error);
  margin: 8px 0 0;
}

.ssh-command {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.ssh-command__text {
  padding: 8px 12px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container-highest);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
  word-break: break-all;
}

.ssh-meta {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-top: 12px;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}
</style>
