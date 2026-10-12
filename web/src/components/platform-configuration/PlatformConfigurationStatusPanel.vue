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
          <button type="button" class="link-button" @click="$emit('viewVersion', status.desired.revision)">revision {{ status.desired.revision }}</button>
          <span class="muted">{{ formatTimestamp(status.desired.createdAt) }} · {{ status.desired.createdBy }}</span>
        </dd>
        <dd v-else class="muted">尚未保存，编辑器使用服务端初始模板</dd>
      </div>
      <div>
        <dt>applied（最后验证）</dt>
        <dd v-if="status.applied">
          <button type="button" class="link-button" @click="$emit('viewVersion', status.applied.revision)">revision {{ status.applied.revision }}</button>
          <span v-if="status.appliedAt" class="muted">验证于 {{ formatTimestamp(status.appliedAt) }}</span>
        </dd>
        <dd v-else class="muted">无</dd>
      </div>
      <div v-if="status.appliedBundleSha256">
        <dt>已观察 Bundle SHA-256</dt>
        <dd><code class="digest">{{ status.appliedBundleSha256 }}</code></dd>
      </div>
    </dl>

    <div class="impact" data-testid="pc-impact">
      <h4>desired 与 applied 的差异</h4>
      <ul v-if="impactItems.length" class="impact-list">
        <li v-for="item in impactItems" :key="item">{{ item }}</li>
      </ul>
      <p v-else class="muted">desired 与最后验证的 applied 在三组设置上没有差异。</p>
      <p class="muted small">这些值目前只保存为配置；清理、提醒、抢占和运行授权消费者尚未接入，保存不会删除文件、发送提醒或停止 Work。</p>
    </div>

    <div class="apply-history" aria-labelledby="pc-apply-heading">
      <h4 id="pc-apply-heading">应用尝试</h4>
      <p v-if="history.length === 0" class="muted">还没有应用请求。</p>
      <article v-for="attempt in history" :key="attempt.id" class="attempt" :data-testid="`pc-attempt-${attempt.id}`">
        <header class="attempt__header">
          <div>
            <strong>{{ attempt.stage === 'failed' ? '失败' : applyStageLabel(attempt.stage) }}</strong>
            <span class="muted">· desired revision {{ attempt.desiredRevision }} · {{ formatTimestamp(attempt.updatedAt) }}</span>
          </div>
          <button type="button" class="text-button" @click="$emit('refreshApply', attempt.id)">刷新</button>
        </header>
        <ol class="stages" :aria-label="`应用阶段：${applyStageLabel(attempt.stage)}`">
          <li
            v-for="stage in APPLY_STAGES"
            :key="stage"
            :class="stageClass(attempt, stage)"
            :aria-current="stage === currentStage(attempt) ? 'step' : undefined"
          >
            {{ applyStageLabel(stage) }}
          </li>
        </ol>
        <p v-if="attempt.failureReason" class="attempt__failure">
          {{ failureReasonLabel(attempt.failureReason) }}<template v-if="attempt.failedStage">（在「{{ applyStageLabel(attempt.failedStage) }}」阶段）</template>
        </p>
        <p class="attempt__next"><span class="muted">下一步：</span>{{ nextStepLabel(attempt.nextStep) }}</p>
        <p class="muted small">
          请求 {{ attempt.requestedBy }} · {{ formatTimestamp(attempt.requestedAt) }} · Bundle <code class="digest">{{ attempt.encryptedBundleSha256 }}</code>
          <template v-if="attempt.executorClientId"> · 执行者 <code>{{ attempt.executorClientId }}</code></template>
        </p>
      </article>
      <PlatformConfigurationFailureNotice v-if="historyFailure" :failure="historyFailure" :dismissible="false" />
      <button
        v-if="history.length && history[history.length - 1].previousApplyId"
        type="button"
        class="outlined-button"
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
import type { PlatformConfigurationApply, PlatformConfigurationApplyStage, PlatformConfigurationStatusSchema } from '@/generated/contracts'
import type { PlatformConfigurationFailure } from '@/composables/usePlatformConfiguration'
import { formatTimestamp } from '@/utils/format'
import { APPLY_STAGES, appliedStateLabel, applyStageLabel, failureReasonLabel, nextStepLabel } from '@/utils/platformConfiguration'
import PlatformConfigurationFailureNotice from './PlatformConfigurationFailureNotice.vue'

const props = defineProps<{
  status: PlatformConfigurationStatusSchema
  history: PlatformConfigurationApply[]
  historyLoading: boolean
  historyFailure: PlatformConfigurationFailure | null
}>()

defineEmits<{
  viewVersion: [revision: number]
  loadPrevious: []
  refreshApply: [applyId: string]
}>()

const applied = computed(() => appliedStateLabel(props.status.appliedState))

const impactItems = computed(() => {
  const items: string[] = []
  if (props.status.impact.runtimePolicyChanged) items.push('运行策略（容器 UID、capabilities、预授权、特权准入）有变化')
  if (props.status.impact.builderPolicyChanged) items.push('构建策略（builder capabilities、提权、entitlements）有变化')
  if (props.status.impact.lifecyclePolicyChanged) items.push('生命周期设置（留存、提醒、抢占宽限）有变化')
  return items
})

function currentStage(attempt: PlatformConfigurationApply): PlatformConfigurationApplyStage {
  return attempt.stage === 'failed' ? (attempt.failedStage ?? 'requested') : attempt.stage
}

function stageClass(attempt: PlatformConfigurationApply, stage: PlatformConfigurationApplyStage): string {
  const order = APPLY_STAGES.indexOf(stage as (typeof APPLY_STAGES)[number])
  const current = APPLY_STAGES.indexOf(currentStage(attempt) as (typeof APPLY_STAGES)[number])
  if (order < current) return 'stage stage--done'
  if (order === current) return attempt.stage === 'failed' ? 'stage stage--failed' : attempt.stage === 'succeeded' ? 'stage stage--done' : 'stage stage--current'
  return 'stage'
}
</script>

<style scoped>
.status-panel { display: grid; gap: 16px; padding: 20px; }
.section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.section-heading h3 { margin: 0; font: var(--md-sys-title-large); color: var(--md-sys-color-on-surface); }
.section-heading p, .applied-detail { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.applied-detail { margin: 0; }
h4 { margin: 0 0 8px; font: var(--md-sys-title-small); color: var(--md-sys-color-on-surface); }
.fact-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 12px 20px; margin: 0; }
.fact-grid dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.fact-grid dd { display: grid; gap: 2px; margin: 4px 0 0; font: var(--md-sys-body-medium); color: var(--md-sys-color-on-surface); }
.digest { overflow-wrap: anywhere; font-size: 12px; }
.muted { color: var(--md-sys-color-on-surface-variant); }
.small { font: var(--md-sys-body-small); margin: 6px 0 0; }
.impact-list { margin: 0; padding-left: 20px; font: var(--md-sys-body-medium); color: var(--md-sys-color-on-surface); }
.state-chip { display: inline-flex; white-space: nowrap; padding: 4px 10px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-medium); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); }
.state-chip--ok { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--warn { background: var(--md-sys-color-tertiary-container, #ffdcc1); color: var(--md-sys-color-on-tertiary-container, #2e1500); }
.apply-history { display: grid; gap: 12px; }
.attempt { display: grid; gap: 8px; padding: 12px 14px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.attempt p { margin: 0; font: var(--md-sys-body-medium); }
.attempt__header { display: flex; justify-content: space-between; align-items: center; gap: 12px; font: var(--md-sys-body-medium); }
.attempt__failure { color: var(--md-sys-color-error); }
.stages { display: flex; flex-wrap: wrap; gap: 6px; margin: 0; padding: 0; list-style: none; }
.stage { padding: 3px 10px; border-radius: var(--md-sys-shape-full); border: 1px solid var(--md-sys-color-outline-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.stage--done { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); border-color: transparent; }
.stage--current { border-color: var(--md-sys-color-primary); color: var(--md-sys-color-primary); }
.stage--failed { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); border-color: transparent; }
.link-button, .text-button { justify-self: start; padding: 0; border: 0; background: transparent; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); cursor: pointer; }
.outlined-button { justify-self: start; min-height: 36px; padding: 0 16px; border: 1px solid var(--md-sys-color-outline); border-radius: var(--md-sys-shape-full); background: transparent; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); cursor: pointer; }
.outlined-button:disabled { opacity: .5; cursor: not-allowed; }
</style>
