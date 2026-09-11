<template>
  <div class="finance-page">
    <header class="page-header">
      <div>
        <h2>预算与费用</h2>
        <p class="page-subtitle">Resource 记录项目预算、用量结算和管理员调整。金额只用于核算，不代表支付。</p>
      </div>
      <button type="button" class="icon-button" aria-label="刷新预算与费用" :disabled="finance.acting !== null" @click="finance.load">
        <SvgIcon name="refresh" size="sm" aria-hidden="true" />
      </button>
    </header>

    <section class="project-strip md-card">
      <label>
        <span>项目</span>
        <select v-model="selectedProjectId" class="text-input" :disabled="projects.projects.kind !== 'success'">
          <option value="">选择项目</option>
          <option v-for="project in projectOptions" :key="project.id" :value="project.id">{{ project.name }} · {{ project.id }}</option>
        </select>
      </label>
      <span v-if="selectedProject" class="project-scope">{{ selectedProject.courseId ? `课程 ${selectedProject.courseId}` : '独立科研项目' }}</span>
    </section>

    <DiagnosticBanner
      v-if="finance.outcome"
      :code="finance.outcome.diagnostic.code"
      :message="finance.outcome.diagnostic.message"
      :retryable="finance.outcome.diagnostic.retryable"
      :severity="finance.outcome.kind === 'error' ? 'error' : 'info'"
      @retry="finance.load"
    />

    <div class="finance-layout">
      <section class="budget-card md-card" aria-labelledby="budget-heading">
        <div class="section-heading">
          <div>
            <h3 id="budget-heading">项目预算</h3>
            <p>预算和已花费由 Resource 服务按当前项目实时计算。</p>
          </div>
        </div>
        <AsyncStateView :state="finance.budget" empty-text="该项目还没有预算记录。" @retry="finance.load">
          <template #success="{ data }">
            <div class="budget-summary">
              <div><span>已花费</span><strong>{{ data.spent.amount }} {{ data.spent.currency }}</strong></div>
              <div><span>预算上限</span><strong>{{ data.limit.amount }} {{ data.limit.currency }}</strong></div>
              <div><span>提醒阈值</span><strong>{{ data.warningAt.amount }} {{ data.warningAt.currency }}</strong></div>
            </div>
            <small class="updated-note">更新于 {{ formatTimestamp(data.updatedAt) }}</small>
          </template>
        </AsyncStateView>
        <form v-if="budgetEditable" class="budget-form" @submit.prevent="saveBudget">
          <label>
            <span>币种</span>
            <input v-model="budgetCurrency" class="text-input" maxlength="32" pattern="[A-Za-z0-9_-]{1,32}" required :readonly="finance.budget.kind === 'success'" />
          </label>
          <label>
            <span>预算上限（{{ budgetCurrency || '币种' }}）</span>
            <input v-model="limitAmount" class="text-input" inputmode="decimal" pattern="(0|[1-9][0-9]*)\.[0-9]{6}" required />
          </label>
          <label>
            <span>提醒阈值（{{ budgetCurrency || '币种' }}）</span>
            <input v-model="warningAmount" class="text-input" inputmode="decimal" pattern="(0|[1-9][0-9]*)\.[0-9]{6}" required />
          </label>
          <button type="submit" class="filled-button" :disabled="!canSaveBudget || finance.acting !== null">{{ finance.budget.kind === 'empty' ? '创建预算' : '保存预算' }}</button>
        </form>
      </section>

      <section class="charges-card md-card" aria-labelledby="charges-heading">
        <div class="section-heading">
          <div>
            <h3 id="charges-heading">费用明细</h3>
            <p>未知或未结算的用量保留原状态，不会显示为零。</p>
          </div>
        </div>
        <AsyncStateView :state="finance.charges" empty-text="该项目暂无费用记录。" @retry="finance.load">
          <template #success="{ data }">
            <ul class="charge-list">
              <li v-for="charge in data" :key="charge.id" class="charge-row">
                  <div class="charge-main">
                    <strong>{{ charge.total.amount }} {{ charge.total.currency }}</strong>
                    <small>{{ charge.id }} · 用量 {{ charge.usageRecordId }}</small>
                    <small>{{ formatTimestamp(charge.createdAt) }} · {{ charge.lines.length }} 个计费项</small>
                    <ul class="charge-line-list" aria-label="费用计算明细">
                      <li v-for="line in charge.lines" :key="`${line.rateId}-${line.rateRevision}-${line.unit}`" class="charge-line">
                        <span>{{ billingUnitLabel(line.unit) }}</span>
                        <small>{{ line.quantity }} / {{ line.unitQuantity }} 基础单位 · 单价 {{ line.unitPrice.amount }} {{ line.unitPrice.currency }}</small>
                        <strong>{{ line.amount.amount }} {{ line.amount.currency }}</strong>
                      </li>
                    </ul>
                    <small v-if="charge.settlement !== 'settled' || charge.diagnosticCode" class="warning-text">
                      <template v-if="charge.diagnosticCode">{{ charge.diagnosticCode }} · </template>
                      当前费用{{ charge.settlement === 'pending' ? '待结算' : '未结算' }}，金额可能继续变化。
                    </small>
                </div>
                <div class="charge-actions">
                  <span class="state-chip" :class="`state-chip--${charge.settlement}`">{{ settlementLabel(charge.settlement) }}</span>
                  <button type="button" class="outlined-button small" @click="selectCharge(charge)">调整</button>
                </div>
              </li>
            </ul>
          </template>
        </AsyncStateView>

        <form v-if="selectedCharge" class="adjustment-form" @submit.prevent="submitAdjustment">
          <div class="section-heading section-heading--compact">
            <div>
              <h4>记录费用调整</h4>
              <p>调整会追加新记录并保留原费用，不会覆盖历史金额。</p>
            </div>
            <button type="button" class="text-button" @click="selectedChargeId = ''">取消</button>
          </div>
          <small>目标费用：{{ selectedCharge.id }} · 原金额 {{ selectedCharge.total.amount }} {{ selectedCharge.total.currency }}</small>
          <div class="two-columns">
            <label>
              <span>调整金额（可为负）</span>
              <input v-model="adjustmentAmount" class="text-input" inputmode="decimal" pattern="-?(0|[1-9][0-9]*)\.[0-9]{6}" placeholder="0.000000" required />
            </label>
            <label>
              <span>币种</span>
              <input :value="selectedCharge.total.currency" class="text-input" readonly />
            </label>
          </div>
          <label>
            <span>调整原因</span>
            <textarea v-model="adjustmentReason" class="text-input" rows="2" maxlength="500" required />
          </label>
          <button type="submit" class="filled-button" :disabled="!canSubmitAdjustment || finance.acting !== null">记录调整</button>
        </form>
      </section>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useProjectResourceFinance, type ResourceCharge } from '@/composables/useProjectResourceFinance'
import { useProjects } from '@/composables/useProjects'
import { formatTimestamp } from '@/utils/format'

const projects = useProjects()
const selectedProjectId = ref<string | null>(null)
const projectOptions = computed(() => projects.projects.kind === 'success' ? projects.projects.data : [])
const selectedProject = computed(() => projectOptions.value.find((project) => project.id === selectedProjectId.value) ?? null)
const finance = useProjectResourceFinance(selectedProjectId)
const budgetCurrency = ref('USD')
const limitAmount = ref('0.000000')
const warningAmount = ref('0.000000')
const selectedChargeId = ref('')
const adjustmentAmount = ref('0.000000')
const adjustmentReason = ref('')

const selectedCharge = computed(() => finance.charges.kind === 'success'
  ? finance.charges.data.find((charge) => charge.id === selectedChargeId.value) ?? null
  : null)
const budgetEditable = computed(() => finance.budget.kind === 'success' || finance.budget.kind === 'empty')
const canSaveBudget = computed(() => {
  if (!budgetEditable.value || !selectedProjectId.value) return false
  const valid = (value: string) => /^(0|[1-9][0-9]*)\.[0-9]{6}$/.test(value)
  return /^[A-Za-z0-9_-]{1,32}$/.test(budgetCurrency.value) && valid(limitAmount.value) && valid(warningAmount.value) && Number(warningAmount.value) <= Number(limitAmount.value)
})
const canSubmitAdjustment = computed(() => Boolean(selectedCharge.value && /^-?(0|[1-9][0-9]*)\.[0-9]{6}$/.test(adjustmentAmount.value) && adjustmentReason.value.trim()))

watch(
  () => projectOptions.value,
  (items) => {
    if (!selectedProjectId.value && items.length > 0) selectedProjectId.value = items[0].id
    if (selectedProjectId.value && !items.some((project) => project.id === selectedProjectId.value)) selectedProjectId.value = items[0]?.id ?? null
  },
  { immediate: true },
)

watch(
  () => finance.budget,
  (state) => {
    if (state.kind === 'success') {
      budgetCurrency.value = state.data.limit.currency
      limitAmount.value = state.data.limit.amount
      warningAmount.value = state.data.warningAt.amount
    } else if (state.kind === 'empty') {
      budgetCurrency.value = 'USD'
      limitAmount.value = '0.000000'
      warningAmount.value = '0.000000'
    }
  },
)

function selectCharge(charge: ResourceCharge) {
  selectedChargeId.value = charge.id
  adjustmentAmount.value = '0.000000'
  adjustmentReason.value = ''
}

async function saveBudget() {
  const project = selectedProject.value
  if (!project || !canSaveBudget.value) return
  await finance.saveBudget({
    projectId: project.id,
    courseId: project.courseId,
    limit: { currency: budgetCurrency.value, amount: limitAmount.value },
    warningAt: { currency: budgetCurrency.value, amount: warningAmount.value },
  })
}

async function submitAdjustment() {
  const charge = selectedCharge.value
  if (!charge || !canSubmitAdjustment.value) return
  const ok = await finance.adjust(charge, {
    amount: { currency: charge.total.currency, amount: adjustmentAmount.value },
    reason: adjustmentReason.value.trim(),
  })
  if (ok) selectedChargeId.value = ''
}

function settlementLabel(value: ResourceCharge['settlement']) {
  return ({ pending: '待结算', settled: '已结算', unsettled: '未结算' } as Record<ResourceCharge['settlement'], string>)[value]
}

function billingUnitLabel(value: ResourceCharge['lines'][number]['unit']) {
  return ({
    cpu_millicore_second: 'CPU',
    memory_byte_second: '内存',
    storage_byte_second: '存储',
    gpu_unit_second: 'GPU',
  } as Record<ResourceCharge['lines'][number]['unit'], string>)[value] ?? value
}
</script>

<style scoped>
.finance-page { display: grid; gap: 20px; }
.page-header, .section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.page-header h2, .section-heading h3, .section-heading h4 { margin: 0; color: var(--md-sys-color-on-surface); }
.page-header h2 { font: var(--md-sys-headline-small); }
.section-heading h3 { font: var(--md-sys-title-large); }
.section-heading h4 { font: var(--md-sys-title-medium); }
.page-subtitle, .section-heading p, .section-heading small, .updated-note { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.project-strip { display: flex; align-items: end; gap: 16px; padding: 16px 20px; }
.project-strip label, .budget-form label, .adjustment-form label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.project-strip label { flex: 1; max-width: 560px; }
.project-scope { padding-bottom: 10px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.finance-layout { display: grid; grid-template-columns: minmax(300px, .75fr) minmax(0, 1.25fr); gap: 20px; align-items: start; }
.budget-card, .charges-card { display: grid; gap: 18px; padding: 20px; }
.budget-summary { display: grid; gap: 10px; grid-template-columns: repeat(3, minmax(0, 1fr)); }
.budget-summary div { display: grid; gap: 5px; padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.budget-summary span { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.budget-summary strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); overflow-wrap: anywhere; }
.budget-form { display: grid; gap: 12px; }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
textarea.text-input { resize: vertical; }
.charge-list { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.charge-row { display: flex; align-items: center; justify-content: space-between; gap: 14px; padding: 14px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.charge-main { display: grid; gap: 4px; min-width: 0; }
.charge-main strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.charge-main small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); overflow-wrap: anywhere; }
.charge-line-list { display: grid; gap: 5px; margin: 7px 0 0; padding: 7px 0 0; border-top: 1px solid var(--md-sys-color-outline-variant); list-style: none; }
.charge-line { display: grid; grid-template-columns: minmax(70px, auto) minmax(0, 1fr) auto; gap: 8px; align-items: baseline; font: var(--md-sys-label-small); }
.charge-line > span { color: var(--md-sys-color-on-surface); }
.charge-line > small { overflow-wrap: anywhere; }
.charge-line > strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-label-small); white-space: nowrap; }
.charge-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; justify-content: flex-end; }
.state-chip { display: inline-flex; white-space: nowrap; padding: 3px 8px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.state-chip--settled { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--pending, .state-chip--unsettled { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.warning-text { color: var(--md-sys-color-error) !important; }
.adjustment-form { display: grid; gap: 12px; padding-top: 18px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.section-heading--compact { align-items: center; }
.two-columns { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
.filled-button, .outlined-button, .text-button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 40px; padding: 0 17px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-large); cursor: pointer; text-decoration: none; }
.filled-button { border: 1px solid var(--md-sys-color-primary); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); }
.outlined-button { border: 1px solid var(--md-sys-color-outline); background: transparent; color: var(--md-sys-color-primary); }
.outlined-button.small { min-height: 32px; padding: 0 11px; font: var(--md-sys-label-medium); }
.text-button { min-height: 32px; padding: 0 8px; border: 0; background: transparent; color: var(--md-sys-color-primary); }
.filled-button:disabled, .outlined-button:disabled, .text-button:disabled { opacity: .5; cursor: not-allowed; }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
@media (max-width: 850px) { .finance-layout { grid-template-columns: 1fr; } .project-strip { align-items: stretch; flex-direction: column; } .project-strip label { max-width: none; } .project-scope { padding-bottom: 0; } }
@media (max-width: 620px) { .budget-summary, .two-columns { grid-template-columns: 1fr; } .charge-row { align-items: flex-start; flex-direction: column; } .charge-actions { justify-content: flex-start; } .charge-line { grid-template-columns: 1fr auto; } .charge-line > small { grid-column: 1 / -1; } }
</style>
