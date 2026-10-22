<template>
  <div class="tab-body">
                  <div
                    v-if="freezeSubmissionPending"
                    class="console-freeze-pane md-card"
                    role="status"
                  >
                    <SvgIcon
                      name="lock"
                      size="lg"
                      aria-hidden="true"
                    />
                    <h4>冻结提交处理中，终端已暂时断开</h4>
                    <p>
                      冻结会读取不可变提交快照，处理期间浏览器终端连接会断开；这是预期行为。请等待提交状态变为“冻结完成”，再重新连接。
                    </p>
                    <button
                      type="button"
                      class="outlined-button"
                      @click="activeTab = teacherMode ? 'operations' : 'freeze'"
                    >
                      {{ teacherMode ? '查看操作状态' : '查看提交状态' }}
                    </button>
                  </div>
                  <div
                    v-else-if="grantUsable && access.grant.kind === 'success'"
                    class="console-wrapper"
                  >
                    <div
                      v-if="consoleReconnectRequired"
                      class="console-freeze-notice"
                      role="status"
                    >
                      <strong>冻结提交可能已断开当前终端连接</strong>
                      <p>
                        冻结状态已结束；现有访问权限仍由当前访问授权控制。点击重新连接会使用同一授权重新建立终端，不会扩大权限范围。
                      </p>
                      <button
                        type="button"
                        class="outlined-button"
                        :disabled="access.creating"
                        @click="reconnectConsole"
                      >
                        重新连接终端
                      </button>
                    </div>
                    <ConsolePanel
                      :key="`console-${consoleSessionKey}-${consolePanelKind}`"
                      :kind="consolePanelKind"
                      :grant="access.grant.data"
                      :environment="data"
                    />
                  </div>
                  <div
                    v-else-if="consoleReconnectRequired && freezeStatus === 'succeeded'"
                    class="console-freeze-pane md-card"
                    role="status"
                  >
                    <SvgIcon
                      name="lock"
                      size="lg"
                      aria-hidden="true"
                    />
                    <h4>冻结完成，终端连接已断开</h4>
                    <p v-if="canIssueAccessGrant(data)">
                      重新签发访问授权后即可继续使用当前环境。现有权限校验保持不变。
                    </p>
                    <button
                      v-if="canIssueAccessGrant(data)"
                      type="button"
                      class="filled-button"
                      :disabled="access.creating"
                      @click="reconnectConsole"
                    >
                      重新签发授权并连接终端
                    </button>
                    <p
                      v-else
                      class="access-grant-hint"
                      role="status"
                    >
                      {{ accessGrantBlockReason(data) }}
                    </p>
                  </div>
                  <div
                    v-else
                    class="console-unauthorized-pane md-card"
                  >
                    <SvgIcon
                      name="terminal"
                      size="lg"
                      aria-hidden="true"
                    />
                    <h4>终端未连接</h4>
                    <p v-if="canIssueAccessGrant(data)">
                      连接云终端需要有效的访问授权。请点击下方按钮一键签发：
                    </p>
                    <button
                      v-if="canIssueAccessGrant(data)"
                      type="button"
                      class="filled-button"
                      :disabled="access.creating"
                      @click="issueAccessGrant"
                    >
                      立即签发授权并连接终端
                    </button>
                    <p
                      v-else
                      class="access-grant-hint"
                      role="status"
                    >
                      {{ accessGrantBlockReason(data) }}
                    </p>
                  </div>
  </div>
</template>

<script setup lang="ts">
import SvgIcon from '@/components/common/SvgIcon.vue'
import ConsolePanel from '@/components/console/ConsolePanel.vue'
import type { EnvironmentInstanceSchema } from '@/generated/contracts'
import { useEnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import { accessGrantBlockReason, canIssueAccessGrant } from '@/utils/environmentLifecycle'

/** Web 控制台：授权后连接终端或桌面；冻结提交期间说明断开原因并提供重新连接。 */
defineProps<{ data: EnvironmentInstanceSchema }>()

const {
  teacherMode,
  activeTab,
  consoleSessionKey,
  consolePanelKind,
  access,
  grantUsable,
  issueAccessGrant,
  reconnectConsole,
  consoleReconnectRequired,
  freezeStatus,
  freezeSubmissionPending,
} = useEnvironmentEntryContext()
</script>

<style scoped>
/* Layout wrapper only: its children stay in the parent's flex layout. */
.tab-body {
  display: contents;
}

.console-unauthorized-pane {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: var(--space-3);
  padding: var(--space-12) var(--space-6);
  text-align: center;
  color: var(--md-sys-color-on-surface-variant);
}

.console-unauthorized-pane h4 {
  font: var(--md-sys-title-medium);
  color: var(--md-sys-color-on-surface);
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

.freeze-status-card p,
.console-freeze-pane p,
.console-freeze-notice p {
  margin: 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
}

.console-freeze-pane {
  align-items: flex-start;
  min-height: 180px;
  justify-content: center;
}

.console-freeze-pane h4 {
  margin: 0;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-medium);
}

.console-freeze-notice {
  margin-bottom: var(--space-4);
  align-items: flex-start;
}

.console-freeze-notice strong {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-small);
}

.access-grant-hint {
  margin: var(--space-3) 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}
</style>
