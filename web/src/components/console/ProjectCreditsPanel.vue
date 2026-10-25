<template>
  <section
    class="credits-card md-card"
    aria-labelledby="credits-heading"
    data-testid="project-credits-panel"
  >
    <div class="section-heading">
      <div>
        <h3 id="credits-heading">
          项目 Credits 账户
        </h3>
        <p>
          项目的 Work、课程实验、构建和模型调用从这个账户预占和扣收 Credits。预算提醒独立于账户，不能代替账户余额。
        </p>
      </div>
      <button
        type="button"
        class="outlined-button small"
        :disabled="credits.acting !== null || !projectId"
        @click="credits.load()"
      >
        刷新账户
      </button>
    </div>

    <DiagnosticBanner
      v-if="credits.outcome"
      :diagnostic="credits.outcome.diagnostic"
      :retryable="credits.outcome.kind === 'error' && credits.outcome.diagnostic.retryable"
      :severity="credits.outcome.kind === 'error' ? 'error' : 'info'"
      @retry="submitAdmin"
    />

    <AsyncStateView
      :state="credits.account"
      empty-text="尚未创建账户。"
      @retry="credits.load()"
    >
      <template #success="{ data }">
        <div
          class="credits-summary"
          data-testid="credits-account-summary"
        >
          <div><span>可用</span><strong>{{ data.available }} {{ data.currency }}</strong></div>
          <div><span>余额</span><strong>{{ data.balance }} {{ data.currency }}</strong></div>
          <div><span>已预占</span><strong>{{ data.reserved }} {{ data.currency }}</strong></div>
          <div><span>已支出</span><strong>{{ data.spent }} {{ data.currency }}</strong></div>
          <div><span>待结算</span><strong>{{ data.pending }} {{ data.currency }}</strong></div>
          <div data-testid="credits-unknown">
            <span>用量待核实</span>
            <strong v-if="data.unknownCount === 0">无</strong>
            <strong v-else-if="isZeroCredits(data.unknown)">{{ data.unknownCount }} 笔，预占已释放</strong>
            <strong v-else>{{ data.unknownCount }} 笔，最多 {{ data.unknown }} {{ data.currency }}</strong>
          </div>
          <div><span>平台承担</span><strong>{{ data.platformAbsorbed }} {{ data.currency }}</strong></div>
        </div>
        <p
          v-if="data.unknownCount > 0"
          class="field-note"
          role="note"
          data-testid="credits-unknown-note"
        >
          有 {{ data.unknownCount }} 笔用量尚未核实。实际费用未知，不按零计算；核实期限到达后预占会释放，迟到的费用由平台承担，不会形成项目欠款。
        </p>
      </template>
    </AsyncStateView>

    <div class="section-heading section-heading--compact">
      <div>
        <h4>账目</h4>
        <p>新记录在前。冲正会追加一条反向记录，原记录保留。</p>
      </div>
    </div>
    <AsyncStateView
      :state="credits.ledger"
      empty-text="该项目还没有 Credits 账目。"
      @retry="credits.loadLedger(credits.ledgerPage)"
    >
      <template #success="{ data }">
        <ul
          class="credit-list"
          aria-label="Credits 账目"
        >
          <li
            v-for="entry in data.items"
            :key="entry.id"
            class="credit-row"
          >
            <div class="credit-main">
              <strong>{{ creditLedgerKindLabel(entry.kind) }} · 余额 {{ signedAmount(entry.balanceDelta) }}</strong>
              <small>预占 {{ signedAmount(entry.reservedDelta) }} · 支出 {{ signedAmount(entry.spentDelta) }}<template v-if="!isZeroCredits(entry.platformAbsorbed)"> · 平台承担 {{ entry.platformAbsorbed }}</template></small>
              <small>{{ entry.reason }} · {{ formatTimestamp(entry.createdAt) }}</small>
              <small v-if="entry.reversalOf">冲正记录</small>
              <details class="advanced-details">
                <summary>查看账目内部标识</summary>
                <small>条目 ID：{{ entry.id }}</small>
                <small>操作 ID：{{ entry.operationId }}</small>
                <small v-if="entry.holdId">预占 ID：{{ entry.holdId }}</small>
                <small v-if="entry.reversalOf">冲正自：{{ entry.reversalOf }}</small>
                <small v-if="entry.actorId">操作者：{{ entry.actorId }}</small>
              </details>
            </div>
            <div
              v-if="canManage && isReversible(entry.kind, entry.reversalOf)"
              class="credit-actions"
            >
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
        <div class="credit-pagination">
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
        <p>资源申请、构建和模型调用开始前会按允许的上限预占 Credits，结束后按实际用量扣收并释放剩余部分。</p>
      </div>
    </div>
    <AsyncStateView
      :state="credits.holds"
      empty-text="该项目暂无 Credits 预占。"
      @retry="credits.loadHolds(credits.holdsPage)"
    >
      <template #success="{ data }">
        <ul
          class="credit-list"
          aria-label="Credits 预占"
        >
          <li
            v-for="hold in data.items"
            :key="hold.id"
            class="credit-row"
          >
            <div class="credit-main">
              <strong>{{ creditHoldStateLabel(hold.state) }} · 剩余 {{ hold.remaining }} / {{ hold.amount }}</strong>
              <small>{{ holdTargetLabel(hold) }}</small>
              <small>{{ formatTimestamp(hold.createdAt) }}</small>
              <template v-if="creditHoldUnknownStatus(hold).kind !== 'none'">
                <span
                  class="state-chip"
                  :class="creditHoldUnknownStatus(hold).kind === 'released' ? 'state-chip--released' : 'state-chip--pending'"
                >{{ holdUnknownMessage(hold) }}</span>
                <small v-if="creditHoldUnknownStatus(hold).kind === 'released'">
                  资金已释放，用量仍待核实；这不代表实例、磁盘或外部调用已经停止。
                </small>
                <small v-else-if="holdUnknownDueAt(hold)">
                  核实截止 {{ formatTimestamp(holdUnknownDueAt(hold)!) }}，到期后释放预占。
                </small>
              </template>
              <details class="advanced-details">
                <summary>查看预占内部标识</summary>
                <small>预占 ID：{{ hold.id }}</small>
                <small>业务键：{{ hold.businessKey }}</small>
                <small>调用方：{{ hold.ownerClientId }}</small>
                <small v-if="hold.requestId">资源申请 ID：{{ hold.requestId }}</small>
                <small v-if="hold.buildRequestId">构建 ID：{{ hold.buildRequestId }}<template v-if="hold.buildAttempt"> · 第 {{ hold.buildAttempt }} 次</template></small>
                <small v-if="hold.modelCallId">模型调用 ID：{{ hold.modelCallId }}</small>
                <small v-if="hold.reason">原因：{{ hold.reason }}</small>
                <small v-if="hold.unknownSince">未知自：{{ formatTimestamp(hold.unknownSince) }}</small>
                <small v-if="hold.expiredUnknownAt">预占释放于：{{ formatTimestamp(hold.expiredUnknownAt) }}</small>
              </details>
            </div>
          </li>
        </ul>
        <div class="credit-pagination">
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

    <p
      v-if="!canManage"
      class="field-note"
      role="note"
      data-testid="credits-read-only-note"
    >
      你可以查看本项目的账户、账目和预占。发放、转移、调整和冲正由平台管理员执行。
    </p>

    <form
      v-else
      class="credit-admin-form"
      data-testid="credits-admin-form"
      @submit.prevent="requestAdmin"
    >
      <div class="section-heading section-heading--compact">
        <div>
          <h4>管理员账务操作</h4>
          <p>每笔操作都需要填写原因，并在账目中留下记录。已预占的资金不能转出或调减。</p>
        </div>
      </div>
      <div
        v-if="credits.pendingOperations.length"
        class="pending-operations"
        role="status"
        data-testid="credits-pending-operations"
      >
        <strong>以下操作的结果尚未确认</strong>
        <small>平台可能已经入账。点击“重新提交”会沿用原请求，已入账的操作不会重复入账；也可以先查看上方账目。</small>
        <ul>
          <li
            v-for="pending in credits.pendingOperations"
            :key="pending.scope"
          >
            <span>{{ pendingSummary(pending.input) }}<template v-if="!pending.persisted">（浏览器未能保存重试记录，刷新后无法安全重试）</template></span>
            <button
              type="button"
              class="text-button"
              :disabled="busy"
              @click="retryPending(pending.scope)"
            >
              重新提交
            </button>
          </li>
        </ul>
      </div>
      <div class="two-columns">
        <label>
          <span>操作</span>
          <AppSelect
            v-model="adminAction"
            aria-label="Credits 操作"
          >
            <option value="grant">发放</option>
            <option value="transfer">转移到其他项目</option>
            <option value="adjust">调整（正数增加，负数减少）</option>
            <option value="reverse">冲正已有账目</option>
          </AppSelect>
        </label>
        <label v-if="adminAction !== 'reverse'">
          <span>金额（CREDITS，最多六位小数）</span>
          <input
            v-model="adminAmount"
            class="text-input"
            aria-label="Credits 金额"
            inputmode="decimal"
            placeholder="例如 20 或 -5.5"
            required
          >
        </label>
        <label v-if="adminAction === 'transfer'">
          <span>目标项目</span>
          <AppSelect
            v-model="destinationProjectId"
            aria-label="目标项目"
            required
          >
            <option value="">选择目标项目</option>
            <option
              v-for="project in destinationOptions"
              :key="project.id"
              :value="project.id"
            >{{ project.name }}</option>
          </AppSelect>
          <button
            v-if="projects.hasMore"
            type="button"
            class="text-button small destination-load-more"
            data-testid="credits-destination-load-more"
            :disabled="projects.loadingMore"
            @click="projects.loadMore()"
          >
            {{ projects.loadingMore ? '加载中…' : '加载更多项目' }}
          </button>
          <DiagnosticBanner
            v-if="projects.loadMoreError"
            :diagnostic="projects.loadMoreError"
            severity="warning"
            @retry="projects.loadMore()"
          />
        </label>
        <label v-if="adminAction === 'reverse'">
          <span>原账目条目 ID</span>
          <input
            v-model="entryId"
            class="text-input"
            aria-label="原账目条目 ID"
            required
          >
        </label>
      </div>
      <small
        v-if="adminAction !== 'reverse' && adminAmount.trim() && !canonicalAmount"
        class="field-note warning-text"
        role="alert"
      >
        {{ adminAction === 'adjust' ? '请输入非零金额，最多六位小数。' : '请输入大于零的金额，最多六位小数。' }}
      </small>
      <label>
        <span>原因</span>
        <textarea
          v-model="adminReason"
          class="text-input"
          aria-label="操作原因"
          rows="2"
          required
        />
      </label>
      <div class="form-actions">
        <button
          type="submit"
          class="filled-button"
          :disabled="!canSubmitAdmin || busy || !projectId"
        >
          {{ adminAction === 'reverse' ? '继续冲正' : adminAction === 'transfer' ? '继续转移' : isDestructive ? '继续调减' : `提交${creditActionLabel(adminAction)}` }}
        </button>
      </div>
    </form>

    <ConfirmDialog
      :open="confirmation !== null"
      :title="confirmation?.title ?? ''"
      :description="confirmation?.description ?? ''"
      :confirm-text="confirmation?.confirmText ?? '确认'"
      severity="error"
      @cancel="confirmation = null"
      @confirm="confirmAdmin"
    />
  </section>
</template>

<script setup lang="ts">
import { computed, reactive, ref, toRef, watch } from 'vue'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import AppSelect from '@/components/form/AppSelect.vue'
import {
  canonicalCreditsAmount,
  creditActionLabel,
  creditHoldStateLabel,
  creditHoldUnknownStatus,
  creditLedgerKindLabel,
  isDestructiveCreditOperation,
  useProjectCredits,
  type CreditAdminAction,
  type CreditAdminFormInput,
  type CreditHold,
} from '@/composables/useProjectCredits'
import { useAuth } from '@/composables/useAuth'
import { useProjects } from '@/composables/useProjects'
import { formatTimestamp } from '@/utils/format'
import { actorIdFromProfile, hasAnyRole, rolesFromProfile } from '@/utils/navigation'

const props = defineProps<{
  projectId: string | null
}>()

const auth = useAuth()
const projects = useProjects()
const credits = reactive(useProjectCredits(toRef(props, 'projectId'), computed(() => actorIdFromProfile(auth.user.value?.profile))))
// The server enforces the platform-admin role on every write; the form is only
// shown to the same role so members never see controls that would be refused.
const canManage = computed(() => hasAnyRole(rolesFromProfile(auth.user.value?.profile), ['admin']))
const adminAction = ref<CreditAdminAction>('grant')
const adminAmount = ref('')
const adminReason = ref('')
const destinationProjectId = ref('')
const entryId = ref('')
const confirmation = ref<{ title: string; description: string; confirmText: string; input: CreditAdminFormInput } | null>(null)
/** The last submitted operation, kept so "重试" replays the same body and Idempotency-Key. */
const lastSubmitted = ref<CreditAdminFormInput | null>(null)
/**
 * Held from sending until the accepted draft is cleared, so a second click
 * cannot resend the same draft between the answer and the cleanup.
 */
const submitting = ref(false)
const busy = computed(() => submitting.value || credits.acting !== null)
/** Bumped on project change; an older submission's cleanup must not touch this project's form. */
let formContext = 0

const destinationOptions = computed(() => projects.projects.kind === 'success'
  ? projects.projects.data.filter((project) => project.id !== props.projectId)
  : [])
const canonicalAmount = computed(() => {
  const amount = canonicalCreditsAmount(adminAmount.value, adminAction.value === 'adjust')
  return amount && amount !== '0.000000' && amount !== '-0.000000' ? amount : null
})
const isDestructive = computed(() => isDestructiveCreditOperation({ action: adminAction.value, amount: canonicalAmount.value ?? '' }))

const canSubmitAdmin = computed(() => {
  if (!canManage.value || !props.projectId || !adminReason.value.trim()) return false
  if (adminAction.value === 'reverse') return Boolean(entryId.value.trim())
  if (!canonicalAmount.value) return false
  return adminAction.value !== 'transfer' || Boolean(destinationProjectId.value)
})

watch(() => props.projectId, () => {
  formContext += 1
  submitting.value = false
  confirmation.value = null
  lastSubmitted.value = null
  destinationProjectId.value = ''
  entryId.value = ''
})

function isZeroCredits(value: string) {
  return /^-?0\.0{6}$/.test(value)
}

function signedAmount(value: string) {
  return value.startsWith('-') || isZeroCredits(value) ? value : `+${value}`
}

/** Holds, releases and unknown markers cannot be reversed (Resource rejects them); reversals are not reversed again. */
function isReversible(kind: string, reversalOf: string | null | undefined) {
  return !reversalOf && kind !== 'hold' && kind !== 'release' && kind !== 'unknown'
}

function holdTargetLabel(hold: CreditHold) {
  if (hold.requestId) return '资源申请'
  if (hold.buildRequestId) return hold.buildAttempt ? `镜像构建（第 ${hold.buildAttempt} 次）` : '镜像构建'
  if (hold.modelCallId) return '模型调用'
  return '其他业务'
}

function holdUnknownMessage(hold: CreditHold): string {
  const status = creditHoldUnknownStatus(hold)
  return status.kind === 'none' ? '' : status.message
}

function holdUnknownDueAt(hold: CreditHold): string | null {
  const status = creditHoldUnknownStatus(hold)
  return status.kind === 'pending' && status.reconciliationDueAt ? status.reconciliationDueAt : null
}

function prepareReverse(id: string) {
  adminAction.value = 'reverse'
  entryId.value = id
}

function requestAdmin() {
  if (!canSubmitAdmin.value || busy.value) return
  const input: CreditAdminFormInput = {
    action: adminAction.value,
    amount: canonicalAmount.value ?? '',
    reason: adminReason.value,
    destinationProjectId: destinationProjectId.value,
    entryId: entryId.value,
  }
  if (!isDestructiveCreditOperation(input)) {
    void execute(input)
    return
  }
  if (input.action === 'transfer') {
    const destination = destinationOptions.value.find((project) => project.id === input.destinationProjectId)?.name ?? input.destinationProjectId
    confirmation.value = {
      title: '确认转移 Credits',
      description: `从当前项目转出 ${input.amount} CREDITS 到“${destination}”。当前项目可用余额会立即减少；要撤回只能再做一次冲正。`,
      confirmText: '转移',
      input,
    }
  } else if (input.action === 'reverse') {
    confirmation.value = {
      title: '确认冲正账目',
      description: `为账目 ${input.entryId} 追加反向记录。原记录保留；如果这笔资金已被预占或消费，冲正会被拒绝。`,
      confirmText: '冲正',
      input,
    }
  } else {
    confirmation.value = {
      title: '确认调减 Credits',
      description: `当前项目余额减少 ${input.amount.slice(1)} CREDITS。已预占的资金不能调减，余额不会低于已预占金额。`,
      confirmText: '调减',
      input,
    }
  }
}

async function confirmAdmin() {
  const pending = confirmation.value
  confirmation.value = null
  if (pending) await execute(pending.input)
}

async function execute(input: CreditAdminFormInput) {
  if (submitting.value) return
  const context = formContext
  submitting.value = true
  lastSubmitted.value = input
  try {
    const result = await credits.submitAdminOperation(input)
    // 'stale': the project or actor changed while this answer was in flight; this draft belongs to another context.
    if (result !== 'accepted' || context !== formContext) return
    lastSubmitted.value = null
    adminReason.value = ''
    adminAmount.value = ''
    if (input.action === 'reverse') entryId.value = ''
  } finally {
    if (context === formContext) submitting.value = false
  }
}

async function retryPending(scope: string) {
  if (submitting.value) return
  const context = formContext
  submitting.value = true
  try {
    await credits.retryPending(scope)
  } finally {
    if (context === formContext) submitting.value = false
  }
}

async function submitAdmin() {
  if (lastSubmitted.value) await execute(lastSubmitted.value)
  else await credits.load()
}

function pendingSummary(input: CreditAdminFormInput) {
  const label = creditActionLabel(input.action)
  if (input.action === 'reverse') return `${label}账目 ${input.entryId} · ${input.reason}`
  const destination = input.action === 'transfer'
    ? ` → ${destinationOptions.value.find((project) => project.id === input.destinationProjectId)?.name ?? input.destinationProjectId}`
    : ''
  return `${label} ${input.amount} CREDITS${destination} · ${input.reason}`
}

defineExpose({ reload: () => credits.load({ clearOutcome: true }) })
</script>

<style scoped>
.credits-card { display: grid; gap: var(--space-4); padding: var(--space-5); }
.section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: var(--space-4); }
.section-heading--compact { align-items: center; }
.section-heading h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-large); }
.section-heading h4 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.section-heading p { margin: var(--space-1) 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.field-note { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); line-height: 1.5; }
.warning-text { color: var(--md-sys-color-error); }
.credits-summary { display: grid; gap: var(--space-3); grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); }
.credits-summary div { display: grid; gap: var(--space-1); padding: var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.credits-summary span { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.credits-summary strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); overflow-wrap: anywhere; }
.credit-list { display: grid; gap: var(--space-2); margin: 0; padding: 0; list-style: none; }
.credit-row { display: flex; align-items: flex-start; justify-content: space-between; gap: var(--space-4); padding: var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.credit-main { display: grid; gap: var(--space-1); min-width: 0; }
.credit-main strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.credit-main small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); overflow-wrap: anywhere; }
.credit-actions { display: flex; align-items: center; gap: var(--space-2); flex-wrap: wrap; justify-content: flex-end; }
.advanced-details { display: grid; gap: var(--space-1); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.advanced-details summary { color: var(--md-sys-color-primary); cursor: pointer; }
.credit-pagination { display: flex; align-items: center; justify-content: flex-end; gap: var(--space-3); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.credit-admin-form { display: grid; gap: var(--space-3); padding-top: var(--space-4); border-top: 1px solid var(--md-sys-color-outline-variant); }
.two-columns { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--space-3); }
.credit-admin-form label { display: grid; gap: var(--space-1); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.form-actions { display: flex; justify-content: flex-end; }
.destination-load-more { justify-self: start; }
.pending-operations { display: grid; gap: var(--space-1); padding: var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); font: var(--md-sys-body-small); }
.pending-operations ul { display: grid; gap: var(--space-1); margin: 0; padding: 0; list-style: none; }
.pending-operations li { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); overflow-wrap: anywhere; }
.state-chip { display: inline-flex; width: fit-content; white-space: nowrap; padding: var(--space-1) var(--space-2); border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.state-chip--pending { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.state-chip--released { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
@media (max-width: 620px) {
  .credits-summary, .two-columns { grid-template-columns: 1fr; }
  .credit-row { flex-direction: column; }
  .credit-actions { justify-content: flex-start; }
}
</style>
