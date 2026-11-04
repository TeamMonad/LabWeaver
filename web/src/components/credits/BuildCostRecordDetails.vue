<template>
  <div
    class="build-cost"
    data-testid="build-cost-record"
  >
    <AsyncStateView
      :state="record"
      loading-text="读取构建费用…"
      empty-text="没有这次构建的费用记录。"
      @retry="load"
    >
      <template #success="{ data }">
        <dl class="build-cost-facts">
          <div>
            <dt>估价</dt>
            <dd data-testid="build-cost-estimate">
              {{ data.estimate.total.amount }} {{ data.estimate.total.currency }}（构建器 {{ data.estimate.input.builderBinding }}，最长 {{ maxDurationLabel(data.estimate.input.maxDurationMilliseconds) }}）
            </dd>
          </div>
          <div>
            <dt>执行授权</dt>
            <dd>
              {{ data.executionAuthorized ? '已获资金授权' : '未获资金授权，不会开始新的构建执行' }}<template v-if="data.executionDeadline">，执行截止 {{ formatTimestamp(data.executionDeadline) }}</template>
            </dd>
          </div>
          <div>
            <dt>实际用时</dt>
            <dd
              data-testid="build-cost-measurement"
              :class="{ 'warning-text': data.measurement?.state !== 'known' }"
            >
              {{ buildMeasurementLabel(data.measurement) }}
            </dd>
          </div>
          <div v-if="data.firstUnknownObservation">
            <dt>首次未知</dt>
            <dd>{{ buildMeasurementLabel(data.firstUnknownObservation) }}</dd>
          </div>
          <div>
            <dt>已结算</dt>
            <dd
              v-if="data.chargedTotal"
              data-testid="build-cost-charged"
            >
              {{ data.chargedTotal.amount }} {{ data.chargedTotal.currency }}
            </dd>
            <dd
              v-else
              class="warning-text"
              data-testid="build-cost-uncharged"
            >
              {{ data.measurement?.state === 'unknown' ? '实际费用未知，不按零计算；核实期限到达后释放预占，迟到费用由平台承担。' : '尚未结算。' }}
            </dd>
          </div>
        </dl>
        <ul
          v-if="data.chargedLines?.length"
          class="build-cost-lines"
          aria-label="构建结算明细"
        >
          <li
            v-for="line in data.chargedLines"
            :key="`${line.rateId}-${line.rateRevision}`"
          >
            费率版本 {{ line.rateRevision }} · {{ line.quantity }}{{ line.quantityDivisor === 1000 ? ' 毫秒' : ' 秒' }} · 单价 {{ line.unitPrice.amount }} {{ line.unitPrice.currency }} / {{ line.unitQuantity }} 秒 · {{ line.amount.amount }} {{ line.amount.currency }}
          </li>
        </ul>
      </template>
    </AsyncStateView>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import type { BuildCostRecordSchema } from '@/generated/contracts'
import { buildMeasurementLabel, fetchBuildCostRecord } from '@/composables/useProjectCredits'
import type { AsyncState } from '@/types/async'
import { formatTimestamp } from '@/utils/format'

const props = defineProps<{
  projectId: string
  buildRequestId: string
  attempt: number
}>()

const record = ref<AsyncState<BuildCostRecordSchema>>({ kind: 'idle' })

async function load() {
  record.value = { kind: 'loading' }
  record.value = await fetchBuildCostRecord(props.projectId, props.buildRequestId, props.attempt)
}

function maxDurationLabel(milliseconds: number) {
  const minutes = milliseconds / 60_000
  return Number.isInteger(minutes) ? `${minutes} 分钟` : `${milliseconds / 1000} 秒`
}

onMounted(load)
</script>

<style scoped>
.build-cost { display: grid; gap: var(--space-2); padding: var(--space-2) var(--space-3); border-left: 2px solid var(--md-sys-color-outline-variant); }
.build-cost-facts { display: grid; gap: var(--space-1); margin: 0; }
.build-cost-facts div { display: grid; grid-template-columns: 5.5em minmax(0, 1fr); gap: var(--space-2); }
.build-cost-facts dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.build-cost-facts dd { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-label-small); overflow-wrap: anywhere; }
.build-cost-lines { display: grid; gap: var(--space-1); margin: 0; padding: 0; list-style: none; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.warning-text { color: var(--md-sys-color-error) !important; }
</style>
