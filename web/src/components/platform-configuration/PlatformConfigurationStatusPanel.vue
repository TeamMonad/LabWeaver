<template>
  <section class="status-panel md-card" aria-labelledby="pc-status-heading">
    <div class="section-heading">
      <div>
        <h3 id="pc-status-heading">当前事实</h3>
        <p>desired 是已保存的期望配置；applied 是最后一次由执行者完整验证通过的版本。</p>
      </div>
      <span class="state-chip" :class="`state-chip--${applied.tone}`" data-testid="pc-applied-state">{{ applied.label }}</span>
    </div>

    <p class="applied-detail">{{ applied.detail }}</p>

    <dl class="fact-grid">
      <div>
        <dt>配置 revision</dt>
        <dd><code>{{ status.revision }}</code></dd>
      </div>
      <div>
        <dt>desired</dt>
        <dd v-if="status.desired">
          <button type="button" class="text-button small" @click="$emit('viewVersion', status.desired.revision)">revision {{ status.desired.revision }}</button>
          <span class="muted">{{ formatTimestamp(status.desired.createdAt) }} · {{ status.desired.createdBy }}</span>
        </dd>
        <dd v-else class="muted">尚未保存，编辑器使用服务端初始模板</dd>
      </div>
      <div>
        <dt>applied（最后验证）</dt>
        <dd v-if="status.applied">
          <button type="button" class="text-button small" @click="$emit('viewVersion', status.applied.revision)">revision {{ status.applied.revision }}</button>
          <span v-if="status.appliedAt" class="muted">验证于 {{ formatTimestamp(status.appliedAt) }}</span>
        </dd>
        <dd v-else class="muted">无</dd>
      </div>
      <div v-if="status.appliedBundleSha256">
        <dt>已观察 Bundle SHA-256</dt>
        <dd><code class="digest">{{ status.appliedBundleSha256 }}</code></dd>
      </div>
      <div data-testid="pc-registered-bundle">
        <dt>已登记 Bundle（当前 desired）</dt>
        <dd v-if="status.registeredBundle">
          <code class="digest">{{ status.registeredBundle.encryptedBundleSha256 }}</code>
          <span class="muted">{{ formatBytes(status.registeredBundle.encryptedBytes) }} · desired revision {{ status.registeredBundle.desiredRevision }} · {{ status.registeredBundle.registeredBy }} · {{ formatTimestamp(status.registeredBundle.registeredAt) }}</span>
        </dd>
        <dd v-else class="muted">无。保存新 desired 会清空登记；提交应用前需重新导出并登记 .age 文件。</dd>
      </div>
    </dl>

    <div class="impact" data-testid="pc-impact">
      <h4>desired 与 applied 的差异</h4>
      <ul v-if="impactItems.length" class="impact-list">
        <li v-for="item in impactItems" :key="item">{{ item }}</li>
      </ul>
      <p v-else class="muted">desired 与最后验证的 applied 在三组设置上没有差异。</p>
      <p class="muted small">差异只说明设置组变化，不代表受影响 Work 数量、重启或费用；清理、提醒、抢占消费者尚未接入，保存不会删除文件、发送提醒或停止 Work。</p>
    </div>

    <div class="apply-history">
      <!-- @vue-generic {PlatformConfigurationApply} -->
      <DataTable
        :columns="historyColumns"
        :rows="history"
        title="应用尝试"
        :counter="history.length"
        aria-label="应用尝试"
        empty-text="还没有应用请求。"
        data-testid="pc-apply-history"
      >
        <template #stage="{ row }">
          <GcpStatusPill domain="custom" :state="row.stage" :label="stageText(row)" size="sm" />
        </template>
        <template #desiredRevision="{ row }">revision {{ row.desiredRevision }}</template>
        <template #nextStep="{ row }">{{ nextStepLabel(row.nextStep) }}</template>
        <template #requestedBy="{ row }">
          {{ row.requestedBy }}
          <span class="muted cell-note">{{ formatTimestamp(row.requestedAt) }}<template v-if="row.executorClientId"> · 执行者 {{ row.executorClientId }}</template></span>
        </template>
        <template #encryptedBundleSha256="{ row }"><code :title="row.encryptedBundleSha256">{{ truncateSha256(row.encryptedBundleSha256) }}</code></template>
        <template #id="{ row }">
          <button type="button" class="text-button small" @click="$emit('refreshApply', row.id)">刷新</button>
        </template>
      </DataTable>
      <DiagnosticBanner v-if="historyFailure" :diagnostic="historyFailure" />
      <button
        v-if="history.length && history[history.length - 1].previousApplyId"
        type="button"
        class="outlined-button small"
        :disabled="historyLoading"
        @click="$emit('loadPrevious')"
      >
        {{ historyLoading ? '读取中…' : '查看上一次尝试' }}
      </button>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import DataTable, { type DataTableColumn } from '@/components/common/DataTable.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import GcpStatusPill from '@/components/common/GcpStatusPill.vue'
import type { PlatformConfigurationApply, PlatformConfigurationStatusSchema } from '@/generated/contracts'
import type { DiagnosticViewModel } from '@/types/async'
import { formatBytes, formatTimestamp, truncateSha256 } from '@/utils/format'
import { appliedStateLabel, applyStageLabel, failureReasonLabel, nextStepLabel } from '@/utils/platformConfiguration'

const props = defineProps<{
  status: PlatformConfigurationStatusSchema
  history: PlatformConfigurationApply[]
  historyLoading: boolean
  historyFailure: DiagnosticViewModel | null
}>()

defineEmits<{
  viewVersion: [revision: number]
  loadPrevious: []
  refreshApply: [applyId: string]
}>()

const historyColumns: DataTableColumn<PlatformConfigurationApply>[] = [
  { key: 'stage', title: '阶段' },
  { key: 'desiredRevision', title: '配置版本' },
  { key: 'nextStep', title: '下一步', minWidth: '200px' },
  { key: 'requestedBy', title: '请求' },
  { key: 'encryptedBundleSha256', title: 'Bundle' },
  { key: 'id', title: '' },
]

const applied = computed(() => appliedStateLabel(props.status.appliedState))

const impactItems = computed(() => {
  const items: string[] = []
  if (props.status.impact.runtimePolicyChanged) items.push('运行策略（特权执行位置等）有变化')
  if (props.status.impact.builderPolicyChanged) items.push('构建策略有变化')
  if (props.status.impact.lifecyclePolicyChanged) items.push('生命周期设置（留存、提醒、抢占宽限）有变化')
  return items
})

/** Real reported stage; a failure names the reason and the stage it failed in. */
function stageText(attempt: PlatformConfigurationApply): string {
  if (attempt.stage !== 'failed') return applyStageLabel(attempt.stage)
  const reason = attempt.failureReason ? failureReasonLabel(attempt.failureReason) : '失败'
  return attempt.failedStage ? `${reason}（${applyStageLabel(attempt.failedStage)}）` : reason
}
</script>

<style scoped>
.status-panel { display: grid; gap: var(--space-4); padding: var(--space-5); min-width: 0; }
.section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: var(--space-4); }
.section-heading h3 { margin: 0; font: var(--md-sys-title-large); color: var(--md-sys-color-on-surface); }
.section-heading p, .applied-detail { margin: var(--space-2) 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.applied-detail { margin: 0; }
h4 { margin: 0 0 var(--space-2); font: var(--md-sys-title-small); color: var(--md-sys-color-on-surface); }
.fact-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: var(--space-3) var(--space-5); margin: 0; }
.fact-grid dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.fact-grid dd { display: grid; justify-items: start; gap: 2px; margin: var(--space-1) 0 0; font: var(--md-sys-body-medium); color: var(--md-sys-color-on-surface); }
.fact-grid .text-button { padding: 0; }
.digest { overflow-wrap: anywhere; font: var(--md-sys-body-small); font-family: var(--md-sys-font-family-mono); }
.muted { color: var(--md-sys-color-on-surface-variant); }
.small { margin: var(--space-2) 0 0; font: var(--md-sys-body-small); }
.cell-note { display: block; font: var(--md-sys-body-small); }
.impact-list { margin: 0; padding-left: var(--space-5); font: var(--md-sys-body-medium); color: var(--md-sys-color-on-surface); }
.state-chip { display: inline-flex; white-space: nowrap; padding: var(--space-1) var(--space-3); border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-medium); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); }
.state-chip--ok { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--warn { background: var(--md-sys-color-warning-container); color: var(--md-sys-color-on-surface); }
.apply-history { display: grid; gap: var(--space-3); justify-items: start; min-width: 0; }
.apply-history > :deep(.data-table) { width: 100%; }
</style>
