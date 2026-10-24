<template>
  <div
    class="estimate-result"
    data-testid="cost-estimate-result"
  >
    <div class="estimate-total">
      <span>估价合计</span>
      <strong data-testid="cost-estimate-total">{{ total.amount }} {{ total.currency }}</strong>
    </div>
    <ul
      class="line-list"
      aria-label="估价明细"
    >
      <li
        v-for="(line, index) in lines"
        :key="`${line.rateId}-${line.rateRevision}-${index}`"
        class="line-row"
      >
        <span>{{ billingUnitLabel(line.unit) }} · {{ formatEstimateQuantity(line) }}</span>
        <strong>{{ line.amount.amount }} {{ line.amount.currency }}</strong>
        <small>单价 {{ line.unitPrice.amount }} {{ line.unitPrice.currency }} / {{ line.unitQuantity }} 基础单位 · 费率版本 {{ line.rateRevision }}</small>
      </li>
    </ul>
    <small class="field-note">合计取平台计算结果。实际扣收按实际用量结算，不超过预占上限；超出预占的部分由平台承担。</small>
  </div>
</template>

<script setup lang="ts">
import {
  billingUnitLabel,
  formatEstimateQuantity,
  type ResourceCostEstimateLine,
} from '@/composables/useProjectResourceCostEstimate'

defineProps<{
  total: { amount: string; currency: string }
  lines: readonly ResourceCostEstimateLine[]
}>()
</script>

<style scoped>
.estimate-result { display: grid; gap: var(--space-2); }
.estimate-total { display: flex; align-items: baseline; justify-content: space-between; gap: var(--space-3); }
.estimate-total span { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.estimate-total strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); overflow-wrap: anywhere; }
.line-list { display: grid; gap: var(--space-1); margin: 0; padding: 0; list-style: none; }
.line-row { display: grid; grid-template-columns: 1fr auto; gap: var(--space-1) var(--space-3); padding: var(--space-2) var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); font: var(--md-sys-body-small); }
.line-row strong { color: var(--md-sys-color-on-surface); }
.line-row small { grid-column: 1 / -1; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.field-note { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); line-height: 1.45; }
</style>
