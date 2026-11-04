<template>
  <li
    class="credit-row"
    :data-hold-id="hold.id"
    data-testid="credit-hold"
  >
    <div class="credit-main">
      <strong>{{ creditHoldStateLabel(hold.state) }} · {{ creditHoldTargetLabel(hold) }}</strong>
      <small>预占上限 {{ hold.amount }} CREDITS · 剩余 {{ hold.remaining }} CREDITS · {{ formatTimestamp(hold.createdAt) }}</small>
      <small
        v-if="modelRates.length"
        data-testid="credit-hold-model-rates"
      >按费率快照计价：{{ modelRates.join('、') }}</small>

      <template v-if="unknown.kind === 'pending'">
        <span class="state-chip state-chip--pending">{{ unknown.message }}</span>
        <small data-testid="credit-hold-unknown">
          实际用量尚未确认，不按零计算<template v-if="hold.modelCallId">（模型调用进行中或尚未上报 token 用量）</template>。
          <template v-if="unknown.reconciliationDueAt">核实截止 {{ formatTimestamp(unknown.reconciliationDueAt) }}，到期后释放剩余预占，之后确认的费用由平台承担。</template>
          <template v-else>服务端未返回核实截止时间，请刷新后再看。</template>
        </small>
      </template>
      <template v-else-if="unknown.kind === 'released'">
        <span class="state-chip state-chip--released">{{ unknown.message }}</span>
        <small data-testid="credit-hold-unknown">
          资金已释放，用量仍待核实<template v-if="unknown.expiredUnknownAt">（释放于 {{ formatTimestamp(unknown.expiredUnknownAt) }}）</template>；之后确认的费用由平台承担，不会形成项目欠款。这不代表实例、磁盘或外部调用已经停止。
        </small>
      </template>

      <small
        v-if="settlement.entries > 0"
        data-testid="credit-hold-settlement"
      >
        结算：扣收 {{ settlement.spent }} CREDITS<template v-if="settlement.platformAbsorbed !== '0.000000'"> · 平台承担 {{ settlement.platformAbsorbed }} CREDITS</template>（按当前账目页中关联本预占的记录）
      </small>
      <small
        v-else-if="hold.state === 'settled'"
        data-testid="credit-hold-settlement"
      >
        已结算；扣收金额见账目中关联本预占的“扣收”记录（不在当前账目页）。
      </small>

      <template v-if="hold.buildRequestId && hold.buildAttempt">
        <button
          type="button"
          class="text-button small"
          :aria-expanded="showBuildCost"
          data-testid="credit-hold-build-cost-toggle"
          @click="showBuildCost = !showBuildCost"
        >
          {{ showBuildCost ? '收起构建费用' : '查看构建费用' }}
        </button>
        <BuildCostRecordDetails
          v-if="showBuildCost"
          :project-id="hold.projectId"
          :build-request-id="hold.buildRequestId"
          :attempt="hold.buildAttempt"
        />
      </template>

      <details class="advanced-details">
        <summary>查看预占内部标识</summary>
        <small>预占 ID：{{ hold.id }}</small>
        <small>业务键：{{ hold.businessKey }}</small>
        <small>调用方：{{ hold.ownerClientId }}</small>
        <small v-if="hold.requestId">资源申请 ID：{{ hold.requestId }}</small>
        <small v-if="hold.buildRequestId">构建 ID：{{ hold.buildRequestId }}<template v-if="hold.buildAttempt"> · 第 {{ hold.buildAttempt }} 次</template></small>
        <small v-if="hold.modelCallId">模型调用 ID：{{ hold.modelCallId }}</small>
        <small
          v-for="snapshot in hold.modelRateSnapshot ?? []"
          :key="snapshot.rateId"
        >{{ billingUnitLabel(snapshot.unit) }}费率 ID：{{ snapshot.rateId }} · 版本 {{ snapshot.rateRevision }}</small>
        <small v-if="hold.reason">原因：{{ hold.reason }}</small>
        <small v-if="hold.unknownSince">未知自：{{ formatTimestamp(hold.unknownSince) }}</small>
        <small v-if="hold.expiredUnknownAt">预占释放于：{{ formatTimestamp(hold.expiredUnknownAt) }}</small>
      </details>
    </div>
  </li>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import BuildCostRecordDetails from '@/components/credits/BuildCostRecordDetails.vue'
import { billingUnitLabel } from '@/composables/useProjectResourceCostEstimate'
import {
  creditHoldSettlement,
  creditHoldStateLabel,
  creditHoldTargetLabel,
  creditHoldUnknownStatus,
  type CreditHold,
  type CreditLedgerEntry,
} from '@/composables/useProjectCredits'
import { formatTimestamp } from '@/utils/format'

const props = defineProps<{
  hold: CreditHold
  /** Ledger entries the panel has loaded; settlement facts are taken only from these. */
  ledgerEntries: readonly CreditLedgerEntry[]
}>()

const showBuildCost = ref(false)
const unknown = computed(() => creditHoldUnknownStatus(props.hold))
const settlement = computed(() => creditHoldSettlement(props.hold.id, props.ledgerEntries))
const modelRates = computed(() => (props.hold.modelRateSnapshot ?? []).map((snapshot) => `${billingUnitLabel(snapshot.unit)} 版本 ${snapshot.rateRevision}`))
</script>

<style scoped>
.credit-row { display: flex; align-items: flex-start; justify-content: space-between; gap: var(--space-4); padding: var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.credit-main { display: grid; gap: var(--space-1); min-width: 0; }
.credit-main strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.credit-main small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); overflow-wrap: anywhere; }
.credit-main .text-button { justify-self: start; }
.advanced-details { display: grid; gap: var(--space-1); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.advanced-details summary { color: var(--md-sys-color-primary); cursor: pointer; }
.state-chip { display: inline-flex; width: fit-content; white-space: nowrap; padding: var(--space-1) var(--space-2); border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.state-chip--pending { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.state-chip--released { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
@media (max-width: 620px) { .credit-row { flex-direction: column; } }
</style>
