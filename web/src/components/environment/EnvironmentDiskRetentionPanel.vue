<template>
  <section
    v-if="visible"
    class="retention-panel md-card"
    aria-labelledby="retention-title"
    data-testid="disk-retention"
    :data-disk-state="disk?.state ?? 'none'"
  >
    <h4 id="retention-title" class="retention-panel__title">磁盘保留与恢复</h4>

    <template v-if="disk">
      <p class="retention-panel__state">
        <strong>{{ DISK_STATE_LABEL[disk.state] }}</strong>
        <span data-testid="retention-compute">{{ computeStatusText(data, disk) }}</span>
      </p>
      <p class="retention-panel__deadline" data-testid="retention-deadline">{{ deadlineText(disk, now) }}</p>
      <p v-if="disk.state === 'retiring' || disk.state === 'retained'" class="retention-panel__note">{{ PLATFORM_PAID_NOTICE }}</p>

      <div v-if="disk.state !== 'purged'" class="retention-inventory" data-testid="retention-inventory">
        <div>
          <h5>仍然保留</h5>
          <ul><li v-for="item in preservedItems(data)" :key="item">{{ item }}</li></ul>
        </div>
        <div>
          <h5>已随计算结束</h5>
          <ul><li v-for="item in lostWithCompute(data)" :key="item">{{ item }}</li></ul>
        </div>
        <div>
          <h5>清除后永久丢失</h5>
          <ul><li v-for="item in lostOnPurge(data)" :key="item">{{ item }}</li></ul>
        </div>
      </div>
      <p class="retention-panel__note">{{ UNAFFECTED_BY_PURGE }}</p>

      <ol v-if="steps.length" class="retention-steps" :aria-label="stepsLabel" data-testid="retention-steps">
        <li v-for="step in steps" :key="step.label" :data-done="step.done ? 'true' : 'false'">
          <SvgIcon :name="step.done ? 'check_circle' : 'radio_button_unchecked'" size="sm" aria-hidden="true" />
          <span class="retention-steps__label">{{ step.label }}</span>
          <span class="retention-steps__status">{{ step.status }}</span>
        </li>
      </ol>

      <p v-if="accepted" class="retention-panel__accepted" role="status" data-testid="disk-recovery-accepted">
        {{ accepted.intent === 'restore' ? '恢复已受理，环境已恢复为“已停止”，不会自动启动。' : '已提交继续清除，平台正在处理。' }}
      </p>

      <DiagnosticBanner
        v-if="purgeFailure"
        :diagnostic="purgeFailure"
        severity="error"
        data-testid="purge-failure"
      />

      <div v-if="offersRestore || offersPurge" class="retention-actions">
        <button
          v-if="offersRestore"
          type="button"
          class="filled-button"
          data-testid="restore-disk"
          :disabled="restoreBlocked !== null"
          @click="requestDiskRecovery(data, 'restore')"
        >
          {{ recoveringDisk ? '正在提交恢复…' : '恢复磁盘' }}
        </button>
        <button
          v-if="offersPurge"
          type="button"
          class="outlined-button danger-button"
          data-testid="continue-purge"
          :disabled="purgeBlocked !== null"
          @click="requestDiskRecovery(data, 'purge')"
        >
          {{ recoveringDisk ? '正在提交…' : '继续清除磁盘' }}
        </button>
        <p v-if="offersRestore && restoreBlocked" class="retention-panel__note" data-testid="restore-blocked">{{ restoreBlocked }}</p>
        <p v-if="offersPurge && purgeBlocked" class="retention-panel__note">{{ purgeBlocked }}</p>
      </div>

      <DiagnosticBanner
        v-if="diskRecoveryDiagnostic"
        :diagnostic="diskRecoveryDiagnostic"
        severity="error"
        data-testid="disk-recovery-diagnostic"
        @retry="retryDiskRecovery"
      />
    </template>
    <p v-else class="retention-panel__state" data-testid="retention-none">
      此环境已删除，平台没有为它保留磁盘（例如创建完成前就已取消）。需要环境时请提交新的资源申请。
    </p>

    <ConfirmDialog
      :open="diskRecoveryConfirmation !== null"
      :title="diskRecoveryConfirmation?.intent === 'purge' ? '继续永久清除磁盘' : '恢复磁盘到新的运行代次'"
      :description="confirmationText"
      :confirm-text="diskRecoveryConfirmation?.intent === 'purge' ? '永久清除' : '恢复磁盘'"
      :severity="diskRecoveryConfirmation?.intent === 'purge' ? 'error' : 'warning'"
      @cancel="diskRecoveryConfirmation = null"
      @confirm="confirmDiskRecovery"
    />
  </section>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import type { EnvironmentDetailSchema } from '@/generated/contracts'
import { useEnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import { formatTimestamp } from '@/utils/format'
import { environmentOperationStateLabel } from '@/utils/stateLabels'
import {
  DISK_STATE_LABEL,
  PLATFORM_PAID_NOTICE,
  UNAFFECTED_BY_PURGE,
  computeStatusText,
  deadlineText,
  failedPurge,
  lostOnPurge,
  lostWithCompute,
  persistedEnvironmentDiagnostic,
  preservedItems,
  purgeConfirmationText,
  restoreConfirmationText,
} from '@/utils/environmentRetention'

/**
 * The Environment's independent disk fact after delete: what is kept and until
 * when, that compute stopped, restore within the window, and continuing a
 * failed purge. Progress comes only from `diskRetention` and the current
 * operation; there are no percentages.
 */
const props = defineProps<{ data: EnvironmentDetailSchema }>()

const {
  diskRecoveryConfirmation,
  diskRecoveryDiagnostic,
  diskRecoveryAccepted,
  recoveringDisk,
  diskRecoveryBlockReason,
  requestDiskRecovery,
  confirmDiskRecovery,
  retryDiskRecovery,
} = useEnvironmentEntryContext()

// The deadline wording follows the clock; a minute is fine-grained enough for a seven-day window.
const now = ref(Date.now())
let timer: ReturnType<typeof setInterval> | undefined
onMounted(() => { timer = setInterval(() => { now.value = Date.now() }, 60_000) })
onBeforeUnmount(() => { if (timer) clearInterval(timer) })

const disk = computed(() => props.data.diskRetention ?? null)
/** Once the restored generation ran again the window is over and the panel steps aside. */
const visible = computed(() => disk.value
  ? !(disk.value.state === 'restored' && disk.value.reactivatedAt)
  : props.data.observedState === 'deleted')

const offersRestore = computed(() => props.data.desiredState === 'deleted'
  && (disk.value?.state === 'retiring' || disk.value?.state === 'retained'))
const offersPurge = computed(() => failedPurge(props.data))
const restoreBlocked = computed(() => diskRecoveryBlockReason(props.data, 'restore', now.value))
const purgeBlocked = computed(() => diskRecoveryBlockReason(props.data, 'purge', now.value))

const accepted = computed(() => diskRecoveryAccepted.value?.environmentId === props.data.id ? diskRecoveryAccepted.value : null)

const purgeFailure = computed(() => {
  const code = props.data.operation.diagnosticCode
  return failedPurge(props.data) && code ? persistedEnvironmentDiagnostic(code) : null
})

const confirmationText = computed(() => {
  const pending = diskRecoveryConfirmation.value
  if (!pending) return ''
  return pending.intent === 'purge' ? purgeConfirmationText(pending.data) : restoreConfirmationText(pending.data)
})

interface Step { label: string; status: string; done: boolean }

const operationStatus = computed(() => {
  const op = props.data.operation
  const attempt = op.state === 'accepted' || op.state === 'running' || op.state === 'failed' ? `，第 ${op.attempt}/${op.maxAttempts} 次尝试` : ''
  return `${environmentOperationStateLabel(op.state)}${attempt}`
})

const stepsLabel = computed(() => {
  switch (disk.value?.state) {
    case 'retiring': return '删除进度'
    case 'restored': return '恢复进度'
    default: return '清除进度'
  }
})

/** Ordered facts from the server; a step is done only when its fact is recorded. */
const steps = computed<Step[]>(() => {
  const value = disk.value
  if (!value) return []
  const op = props.data.operation
  switch (value.state) {
    case 'retiring':
      return [
        { label: '删除已受理', status: formatTimestamp(value.startedAt), done: true },
        { label: '停止计算', status: value.computeRetiredAt ? formatTimestamp(value.computeRetiredAt) : operationStatus.value, done: Boolean(value.computeRetiredAt) },
        { label: '保留磁盘', status: `至 ${formatTimestamp(value.expiresAt)}`, done: false },
      ]
    case 'restored':
      return [
        { label: '恢复已受理', status: op.kind === 'restore' ? environmentOperationStateLabel(op.state) : '已完成', done: true },
        { label: `磁盘挂回第 ${value.restoredGeneration ?? props.data.generation} 代，环境已停止`, status: '已完成', done: true },
        {
          label: '启动计算',
          status: op.kind === 'start' ? operationStatus.value : '等待启动；需要新的计算资金授权',
          done: false,
        },
      ]
    case 'purging':
    case 'purged':
      return [
        { label: '保留期结束', status: formatTimestamp(value.expiresAt), done: true },
        { label: '清除磁盘', status: value.state === 'purged' ? '已完成' : op.kind === 'purge' ? operationStatus.value : '等待处理', done: value.state === 'purged' },
        { label: '清除完成', status: value.purgedAt ? formatTimestamp(value.purgedAt) : '尚未完成', done: Boolean(value.purgedAt) },
      ]
    default:
      return []
  }
})
</script>

<style scoped>
.retention-panel { display: grid; gap: var(--space-3); padding: var(--space-4); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.retention-panel__title { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.retention-panel__state { display: flex; flex-wrap: wrap; gap: var(--space-1) var(--space-2); margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.retention-panel__deadline { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.retention-panel__note { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.retention-panel__accepted { margin: 0; color: var(--md-sys-color-primary); font: var(--md-sys-body-small); }
.retention-inventory { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: var(--space-3); }
.retention-inventory h5 { margin: 0 0 var(--space-1); color: var(--md-sys-color-on-surface); font: var(--md-sys-label-large); }
.retention-inventory ul { margin: 0; padding-left: 18px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.retention-steps { display: grid; gap: var(--space-1); margin: 0; padding: 0; list-style: none; font: var(--md-sys-body-small); }
.retention-steps li { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2); color: var(--md-sys-color-on-surface-variant); }
.retention-steps li[data-done='true'] { color: var(--md-sys-color-on-surface); }
.retention-steps__label { font-weight: 500; }
.retention-actions { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2) var(--space-4); }
</style>
