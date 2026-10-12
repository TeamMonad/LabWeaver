<template>
  <section
    class="credits-card md-card"
    aria-labelledby="credits-heading"
  >
    <div class="section-heading">
      <div>
        <h3 id="credits-heading">
          项目 Credits 账户
        </h3>
        <p>
          余额与预占用于资源准入；预算提醒独立存在，不能替代账户余额。金额为六位小数字符串，页面不做浮点换算。
        </p>
      </div>
      <button
        type="button"
        class="outlined-button small"
        :disabled="credits.acting !== null || !projectId"
        @click="credits.load"
      >
        刷新账户
      </button>
    </div>

    <DiagnosticBanner
      v-if="credits.outcome"
      :code="credits.outcome.diagnostic.code"
      :message="credits.outcome.diagnostic.message"
      :retryable="credits.outcome.diagnostic.retryable"
      :severity="credits.outcome.kind === 'error' ? 'error' : 'info'"
      @retry="credits.load"
    />

    <AsyncStateView
      :state="credits.account"
      empty-text="尚未创建账户；首次发放或预占时会自动建立，读取为零。"
      @retry="credits.load"
    >
      <template #success="{ data }">
        <div
          class="budget-summary"
          data-testid="credits-account-summary"
        >
          <div><span>可用</span><strong>{{ data.available }} {{ data.currency }}</strong></div>
          <div><span>余额</span><strong>{{ data.balance }} {{ data.currency }}</strong></div>
          <div><span>预占</span><strong>{{ data.reserved }} {{ data.currency }}</strong></div>
          <div><span>已支出</span><strong>{{ data.spent }} {{ data.currency }}</strong></div>
          <div><span>待结算预占</span><strong>{{ data.pending }} {{ data.currency }}</strong></div>
          <div>
            <span>未知用量</span>
            <strong>{{ data.unknown }} {{ data.currency }}</strong>
            <small v-if="data.unknownCount > 0">待核实 {{ data.unknownCount }} 笔（释放后仍可能保留）</small>
          </div>
          <div><span>平台承担</span><strong>{{ data.platformAbsorbed }} {{ data.currency }}</strong></div>
        </div>
        <small class="updated-note">账户修订 {{ data.revision }}</small>
        <DiagnosticBanner
          v-if="data.unknownCount > 0"
          code="CREDITS_UNKNOWN_PENDING"
          severity="warning"
          :retryable="false"
          :message="`仍有 ${data.unknownCount} 笔预占待核实未知用量。未知金额可能为 0，但计数表示对账尚未完成。`"
        />
      </template>
    </AsyncStateView>

    <div class="section-heading section-heading--compact">
      <div>
        <h4>账目</h4>
        <p>按创建时间倒序；冲正追加新记录，不覆盖原文。</p>
      </div>
    </div>
    <AsyncStateView
      :state="credits.ledger"
      empty-text="该项目还没有 Credits 账目。"
      @retry="credits.loadLedger(credits.ledgerPage)"
    >
      <template #success="{ data }">
        <ul class="charge-list">
          <li
            v-for="entry in data.items"
            :key="entry.id"
            class="charge-row"
          >
            <div class="charge-main">
              <strong>{{ entry.kind }} · 余额 {{ entry.balanceDelta }}</strong>
              <small>预占 {{ entry.reservedDelta }} · 支出 {{ entry.spentDelta }} · 平台 {{ entry.platformAbsorbed }}</small>
              <small>{{ entry.reason }} · {{ formatTimestamp(entry.createdAt) }}</small>
              <details class="advanced-details">
                <summary>查看账目内部标识</summary>
                <small>条目 ID：{{ entry.id }}</small>
                <small>操作 ID：{{ entry.operationId }}</small>
                <small v-if="entry.holdId">预占 ID：{{ entry.holdId }}</small>
                <small v-if="entry.reversalOf">冲正自：{{ entry.reversalOf }}</small>
                <small v-if="entry.actorId">操作者：{{ entry.actorId }}</small>
              </details>
            </div>
            <div class="charge-actions">
              <button
                type="button"
                class="text-button"
                :disabled="credits.acting !== null"
                @click="prepareReverse(entry.id)"
              >
                冲正此条
              </button>
            </div>
          </li>
        </ul>
        <div class="usage-pagination">
          <button
            type="button"
            class="outlined-button small"
            :disabled="data.page <= 1 || credits.acting !== null"
            @click="credits.loadLedger(data.page - 1)"
          >
            上一页
          </button>
          <span>第 {{ data.page }} 页</span>
          <button
            type="button"
            class="outlined-button small"
            :disabled="!data.hasMore || credits.acting !== null"
            @click="credits.loadLedger(data.page + 1)"
          >
            下一页
          </button>
        </div>
      </template>
    </AsyncStateView>

    <div class="section-heading section-heading--compact">
      <div>
        <h4>预占</h4>
        <p>显示业务键、申请与剩余；未知用量在核实期限到期后显示“待核实，预占已释放”，unknownCount 仍可大于 0。</p>
      </div>
    </div>
    <AsyncStateView
      :state="credits.holds"
      empty-text="该项目暂无 Credits 预占。"
      @retry="credits.loadHolds(credits.holdsPage)"
    >
      <template #success="{ data }">
        <ul class="charge-list">
          <li
            v-for="hold in data.items"
            :key="hold.id"
            class="charge-row"
          >
            <div class="charge-main">
              <strong>{{ hold.state }} · 剩余 {{ hold.remaining }} / {{ hold.amount }}</strong>
              <small>{{ hold.businessKey }} · 申请 {{ hold.requestId }}</small>
              <small>{{ formatTimestamp(hold.createdAt) }}</small>
              <template v-if="holdUnknownKind(hold) !== 'none'">
                <span
                  class="state-chip"
                  :class="holdUnknownKind(hold) === 'released' ? 'state-chip--released' : 'state-chip--pending'"
                >{{ holdUnknownMessage(hold) }}</span>
                <small v-if="holdUnknownKind(hold) === 'released'">
                  资金已释放，未知事实仍保留；不代表实例、磁盘或外部调用已停止。
                </small>
                <small v-else-if="holdUnknownDueAt(hold)">
                  核实截止 {{ formatTimestamp(holdUnknownDueAt(hold)!) }}
                </small>
              </template>
              <details class="advanced-details">
                <summary>查看预占内部标识</summary>
                <small>预占 ID：{{ hold.id }}</small>
                <small>调用方：{{ hold.ownerClientId }}</small>
                <small v-if="hold.reason">原因：{{ hold.reason }}</small>
                <small v-if="hold.unknownSince">未知自：{{ formatTimestamp(hold.unknownSince) }}</small>
                <small v-if="hold.reconciliationDueAt">核实截止：{{ formatTimestamp(hold.reconciliationDueAt) }}</small>
                <small v-if="hold.expiredUnknownAt">预占释放于：{{ formatTimestamp(hold.expiredUnknownAt) }}</small>
              </details>
            </div>
          </li>
        </ul>
        <div class="usage-pagination">
          <button
            type="button"
            class="outlined-button small"
            :disabled="data.page <= 1 || credits.acting !== null"
            @click="credits.loadHolds(data.page - 1)"
          >
            上一页
          </button>
          <span>第 {{ data.page }} 页</span>
          <button
            type="button"
            class="outlined-button small"
            :disabled="!data.hasMore || credits.acting !== null"
            @click="credits.loadHolds(data.page + 1)"
          >
            下一页
          </button>
        </div>
      </template>
    </AsyncStateView>

    <form
      class="adjustment-form"
      data-testid="credits-admin-form"
      @submit.prevent="submitAdmin"
    >
      <div class="section-heading section-heading--compact">
        <div>
          <h4>管理员账务操作</h4>
          <p>仅平台管理员可写。须提供稳定幂等键（自动生成）与原因；转移与冲正在资金已预占时可能返回 409。</p>
        </div>
      </div>
      <div class="two-columns">
        <label>
          <span>操作</span>
          <select
            v-model="adminAction"
            class="text-input"
          >
            <option value="grant">发放 grant</option>
            <option value="transfer">转移 transfer</option>
            <option value="adjust">调整 adjust</option>
            <option value="reverse">冲正 reverse</option>
          </select>
        </label>
        <label v-if="adminAction !== 'reverse'">
          <span>金额（六位小数）</span>
          <input
            v-model="adminAmount"
            class="text-input"
            inputmode="decimal"
            :pattern="adminAction === 'adjust' ? '-?(0|[1-9][0-9]*)\\.[0-9]{6}' : '(0|[1-9][0-9]*)\\.[0-9]{6}'"
            required
          >
        </label>
        <label v-if="adminAction === 'transfer'">
          <span>目标项目 ID</span>
          <input
            v-model="destinationProjectId"
            class="text-input"
            required
          >
        </label>
        <label v-if="adminAction === 'reverse'">
          <span>原账目条目 ID</span>
          <input
            v-model="entryId"
            class="text-input"
            required
          >
        </label>
      </div>
      <label>
        <span>原因</span>
        <textarea
          v-model="adminReason"
          class="text-input"
          rows="2"
          required
        />
      </label>
      <button
        type="submit"
        class="filled-button"
        :disabled="!canSubmitAdmin || credits.acting !== null || !projectId"
      >
        提交管理员操作
      </button>
    </form>
  </section>
</template>

<script setup lang="ts">
import { computed, reactive, ref, toRef } from 'vue'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import {
  creditHoldUnknownStatus,
  isCreditsFixedDecimal,
  isPositiveCreditsAmount,
  useProjectCredits,
  type CreditAdminAction,
} from '@/composables/useProjectCredits'
import { formatTimestamp } from '@/utils/format'

const props = defineProps<{
  projectId: string | null
}>()

const credits = reactive(useProjectCredits(toRef(props, 'projectId')))
const adminAction = ref<CreditAdminAction>('grant')
const adminAmount = ref('20.000000')
const adminReason = ref('')
const destinationProjectId = ref('')
const entryId = ref('')

const canSubmitAdmin = computed(() => {
  if (!props.projectId || !adminReason.value.trim()) return false
  if (adminAction.value === 'reverse') return Boolean(entryId.value.trim())
  if (adminAction.value === 'transfer') {
    return isPositiveCreditsAmount(adminAmount.value)
      && Boolean(destinationProjectId.value.trim())
      && destinationProjectId.value.trim() !== props.projectId
  }
  if (adminAction.value === 'adjust') {
    return isCreditsFixedDecimal(adminAmount.value)
      && adminAmount.value !== '0.000000'
      && adminAmount.value !== '-0.000000'
  }
  return isPositiveCreditsAmount(adminAmount.value)
})

function holdUnknownKind(hold: Parameters<typeof creditHoldUnknownStatus>[0]) {
  return creditHoldUnknownStatus(hold).kind
}

function holdUnknownMessage(hold: Parameters<typeof creditHoldUnknownStatus>[0]): string {
  const status = creditHoldUnknownStatus(hold)
  return status.kind === 'none' ? '' : status.message
}

function holdUnknownDueAt(hold: Parameters<typeof creditHoldUnknownStatus>[0]): string | null {
  const status = creditHoldUnknownStatus(hold)
  return status.kind === 'pending' && status.reconciliationDueAt ? status.reconciliationDueAt : null
}

function prepareReverse(id: string) {
  adminAction.value = 'reverse'
  entryId.value = id
  adminReason.value = adminReason.value.trim() || '冲正错误账目'
}

async function submitAdmin() {
  if (!canSubmitAdmin.value) return
  const ok = await credits.submitAdminOperation({
    action: adminAction.value,
    amount: adminAmount.value,
    reason: adminReason.value,
    destinationProjectId: destinationProjectId.value,
    entryId: entryId.value,
  })
  if (ok) {
    adminReason.value = ''
    if (adminAction.value === 'reverse') entryId.value = ''
  }
}

defineExpose({ reload: () => credits.load({ clearOutcome: true }) })
</script>

<style scoped>
.credits-card { display: grid; gap: 18px; padding: 20px; }
.section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.section-heading--compact { align-items: center; }
.section-heading h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-large); }
.section-heading h4 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.section-heading p, .updated-note { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.budget-summary { display: grid; gap: 10px; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); }
.budget-summary div { display: grid; gap: 5px; padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.budget-summary span { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.budget-summary strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); white-space: nowrap; }
.budget-summary small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.charge-list { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.charge-row { display: flex; align-items: flex-start; justify-content: space-between; gap: 14px; padding: 14px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.charge-main { display: grid; gap: 4px; min-width: 0; }
.charge-main strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.charge-main small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); overflow-wrap: anywhere; }
.charge-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; justify-content: flex-end; }
.advanced-details { display: grid; gap: 4px; margin-top: 2px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.advanced-details summary { color: var(--md-sys-color-primary); cursor: pointer; }
.usage-pagination { display: flex; align-items: center; justify-content: flex-end; gap: 10px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.adjustment-form { display: grid; gap: 12px; padding-top: 18px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.two-columns { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
.adjustment-form label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
textarea.text-input { resize: vertical; }
.state-chip { display: inline-flex; white-space: nowrap; padding: 3px 8px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); width: fit-content; }
.state-chip--pending { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.state-chip--released { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.filled-button, .outlined-button, .text-button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 40px; padding: 0 17px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-large); cursor: pointer; text-decoration: none; }
.filled-button { border: 1px solid var(--md-sys-color-primary); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); }
.outlined-button { border: 1px solid var(--md-sys-color-outline); background: transparent; color: var(--md-sys-color-primary); }
.outlined-button.small { min-height: 32px; padding: 0 11px; font: var(--md-sys-label-medium); }
.text-button { min-height: 32px; padding: 0 8px; border: 0; background: transparent; color: var(--md-sys-color-primary); }
.filled-button:disabled, .outlined-button:disabled, .text-button:disabled { opacity: .5; cursor: not-allowed; }
@media (max-width: 620px) {
  .budget-summary, .two-columns { grid-template-columns: 1fr; }
  .charge-row { align-items: flex-start; flex-direction: column; }
  .charge-actions { justify-content: flex-start; }
}
</style>
