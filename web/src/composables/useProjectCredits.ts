import { onScopeDispose, ref, watch, type Ref } from 'vue'
import {
  createCreditAdminOperation,
  getProjectCredits,
  listProjectCreditHolds,
  listProjectCreditLedger,
  type CreditAccountSchema,
  type CreditAdminOperationSchema,
  type CreditHold,
  type CreditHoldPageSchema,
  type CreditLedgerEntry,
  type CreditLedgerPageSchema,
} from '@/generated/contracts'
import { diagnosticFromError, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey } from '@/utils/format'

const fixedDecimalPattern = /^-?(0|[1-9][0-9]*)\.[0-9]{6}$/

export function isCreditsFixedDecimal(value: string): boolean {
  return fixedDecimalPattern.test(value)
}

export function isPositiveCreditsAmount(value: string): boolean {
  return /^(0|[1-9][0-9]*)\.[0-9]{6}$/.test(value) && value !== '0.000000'
}

/**
 * Pad, never round, a typed amount into the contract's exact six-decimal
 * string ("20" → "20.000000"). Returns null for anything with more than six
 * decimals or a sign the operation does not allow.
 */
export function canonicalCreditsAmount(value: string, allowNegative: boolean): string | null {
  const trimmed = value.trim()
  const match = /^(-?)(0|[1-9][0-9]*)(?:\.([0-9]{1,6}))?$/.exec(trimmed)
  if (!match || (match[1] && !allowNegative)) return null
  return `${match[1]}${match[2]}.${(match[3] ?? '').padEnd(6, '0')}`
}

export type CreditHoldUnknownStatus =
  | { kind: 'none' }
  | { kind: 'pending'; message: string; reconciliationDueAt?: string | null; unknownSince?: string | null }
  | { kind: 'released'; message: string; expiredUnknownAt?: string | null; unknownSince?: string | null }

/** Classify unknown-measurement holds for UI. Never treat unknown as free/zero. */
export function creditHoldUnknownStatus(hold: CreditHold): CreditHoldUnknownStatus {
  if (!hold.measurementUnknown) return { kind: 'none' }
  const released =
    hold.state === 'released'
    || (typeof hold.expiredUnknownAt === 'string' && hold.expiredUnknownAt.length > 0)
  if (released) {
    return {
      kind: 'released',
      message: '待核实，预占已释放',
      expiredUnknownAt: hold.expiredUnknownAt,
      unknownSince: hold.unknownSince,
    }
  }
  return {
    kind: 'pending',
    message: '用量未知待核实',
    reconciliationDueAt: hold.reconciliationDueAt,
    unknownSince: hold.unknownSince,
  }
}

export type CreditAdminAction = CreditAdminOperationSchema['action']

export interface CreditAdminFormInput {
  action: CreditAdminAction
  amount: string
  reason: string
  destinationProjectId: string
  entryId: string
}

/** Operations that take Credits away from a project or undo a booked entry; the UI confirms them first. */
export function isDestructiveCreditOperation(input: Pick<CreditAdminFormInput, 'action' | 'amount'>): boolean {
  if (input.action === 'transfer' || input.action === 'reverse') return true
  return input.action === 'adjust' && input.amount.trim().startsWith('-')
}

const creditActionLabels: Record<CreditAdminAction, string> = {
  grant: '发放',
  transfer: '转移',
  adjust: '调整',
  reverse: '冲正',
}

export function creditActionLabel(action: CreditAdminAction): string {
  return creditActionLabels[action]
}

const creditHoldStateLabels: Record<CreditHold['state'], string> = {
  pending: '预占中',
  unknown: '用量待核实',
  settled: '已结算',
  released: '已释放',
}

export function creditHoldStateLabel(state: string): string {
  return creditHoldStateLabels[state as CreditHold['state']] ?? `未识别状态（${state}）`
}

/**
 * Ledger kinds written by Resource credits. An unrecognised kind is shown
 * verbatim so a new server kind is never mislabelled as a known one.
 */
const creditLedgerKindLabels: Record<string, string> = {
  grant: '管理员发放',
  transfer: '转移',
  adjust: '管理员调整',
  reverse: '冲正',
  hold: '预占',
  release: '释放预占',
  settle: '扣收',
  unknown: '用量待核实',
}

export function creditLedgerKindLabel(kind: string): string {
  return creditLedgerKindLabels[kind] ?? kind
}

export function useProjectCredits(projectId: Ref<string | null>) {
  const account = ref<AsyncState<CreditAccountSchema>>({ kind: 'idle' })
  const ledger = ref<AsyncState<CreditLedgerPageSchema>>({ kind: 'idle' })
  const holds = ref<AsyncState<CreditHoldPageSchema>>({ kind: 'idle' })
  const ledgerPage = ref(1)
  const holdsPage = ref(1)
  const pageSize = ref(25)
  const acting = ref<string | null>(null)
  const outcome = ref<{ kind: 'success' | 'error'; diagnostic: DiagnosticViewModel } | null>(null)
  let loadGeneration = 0
  let disposed = false
  // One Idempotency-Key per operation body. A failed or lost response keeps the
  // key, so resubmitting the same operation replays it instead of booking twice.
  let pendingOperation: { fingerprint: string; key: string } | null = null

  onScopeDispose(() => {
    disposed = true
  })

  function current(id: string, generation: number) {
    return !disposed && projectId.value === id && generation === loadGeneration
  }

  async function loadLedgerPage(id: string, page: number, generation: number) {
    ledger.value = { kind: 'loading', message: '加载 Credits 账目…' }
    try {
      const result = await listProjectCreditLedger({
        path: { projectId: id },
        query: { page, pageSize: pageSize.value },
      })
      if (!current(id, generation)) return
      if (result.error) throw result.error
      const pageData = result.data
      ledger.value = pageData.items.length || pageData.page > 1
        ? { kind: 'success', data: pageData }
        : { kind: 'empty' }
      ledgerPage.value = pageData.page
    } catch (error) {
      if (!current(id, generation)) return
      ledger.value = {
        kind: 'error',
        diagnostic: diagnosticFromError(error, 'CREDITS_LEDGER_LOAD_FAILED', '无法加载 Credits 账目。'),
      }
    }
  }

  async function loadHoldsPage(id: string, page: number, generation: number) {
    holds.value = { kind: 'loading', message: '加载 Credits 预占…' }
    try {
      const result = await listProjectCreditHolds({
        path: { projectId: id },
        query: { page, pageSize: pageSize.value },
      })
      if (!current(id, generation)) return
      if (result.error) throw result.error
      const pageData = result.data
      holds.value = pageData.items.length || pageData.page > 1
        ? { kind: 'success', data: pageData }
        : { kind: 'empty' }
      holdsPage.value = pageData.page
    } catch (error) {
      if (!current(id, generation)) return
      holds.value = {
        kind: 'error',
        diagnostic: diagnosticFromError(error, 'CREDITS_HOLDS_LOAD_FAILED', '无法加载 Credits 预占。'),
      }
    }
  }

  async function load(options: { clearOutcome?: boolean } = {}) {
    const id = projectId.value
    const generation = ++loadGeneration
    if (options.clearOutcome !== false) outcome.value = null
    if (!id) {
      account.value = { kind: 'idle' }
      ledger.value = { kind: 'idle' }
      holds.value = { kind: 'idle' }
      ledgerPage.value = 1
      holdsPage.value = 1
      return
    }
    account.value = { kind: 'loading', message: '加载 Credits 账户…' }
    ledgerPage.value = 1
    holdsPage.value = 1
    try {
      const result = await getProjectCredits({ path: { projectId: id } })
      if (!current(id, generation)) return
      if (result.error) throw result.error
      account.value = { kind: 'success', data: result.data }
    } catch (error) {
      if (!current(id, generation)) return
      account.value = {
        kind: 'error',
        diagnostic: diagnosticFromError(error, 'CREDITS_ACCOUNT_LOAD_FAILED', '无法加载 Credits 账户。'),
      }
    }
    await Promise.all([
      loadLedgerPage(id, 1, generation),
      loadHoldsPage(id, 1, generation),
    ])
  }

  async function loadLedger(page: number) {
    const id = projectId.value
    if (!id) return
    await loadLedgerPage(id, page, loadGeneration)
  }

  async function loadHolds(page: number) {
    const id = projectId.value
    if (!id) return
    await loadHoldsPage(id, page, loadGeneration)
  }

  async function submitAdminOperation(input: CreditAdminFormInput): Promise<boolean> {
    const id = projectId.value
    if (!id || acting.value !== null) return false
    const reason = input.reason.trim()
    if (!reason) {
      outcome.value = {
        kind: 'error',
        diagnostic: makeDiagnostic('CREDITS_REASON_REQUIRED', '管理员操作必须填写原因。', false),
      }
      return false
    }

    let body: CreditAdminOperationSchema
    if (input.action === 'grant') {
      if (!isPositiveCreditsAmount(input.amount)) {
        outcome.value = {
          kind: 'error',
          diagnostic: makeDiagnostic('CREDITS_AMOUNT_INVALID', '发放金额必须是大于零的六位小数字符串。', false),
        }
        return false
      }
      body = { action: 'grant', projectId: id, amount: input.amount, reason }
    } else if (input.action === 'transfer') {
      if (!isPositiveCreditsAmount(input.amount)) {
        outcome.value = {
          kind: 'error',
          diagnostic: makeDiagnostic('CREDITS_AMOUNT_INVALID', '转移金额必须是大于零的六位小数字符串。', false),
        }
        return false
      }
      const destinationProjectId = input.destinationProjectId.trim()
      if (!destinationProjectId || destinationProjectId === id) {
        outcome.value = {
          kind: 'error',
          diagnostic: makeDiagnostic('CREDITS_DESTINATION_INVALID', '转移必须指定与当前项目不同的目标项目。', false),
        }
        return false
      }
      body = {
        action: 'transfer',
        projectId: id,
        destinationProjectId,
        amount: input.amount,
        reason,
      }
    } else if (input.action === 'adjust') {
      if (!isCreditsFixedDecimal(input.amount) || input.amount === '0.000000' || input.amount === '-0.000000') {
        outcome.value = {
          kind: 'error',
          diagnostic: makeDiagnostic('CREDITS_AMOUNT_INVALID', '调整金额必须是非零的六位小数字符串。', false),
        }
        return false
      }
      body = { action: 'adjust', projectId: id, amount: input.amount, reason }
    } else {
      const entryId = input.entryId.trim()
      if (!entryId) {
        outcome.value = {
          kind: 'error',
          diagnostic: makeDiagnostic('CREDITS_ENTRY_REQUIRED', '冲正必须指定原账目条目 ID。', false),
        }
        return false
      }
      body = { action: 'reverse', entryId, reason }
    }

    acting.value = input.action
    outcome.value = null
    const fingerprint = JSON.stringify(body)
    if (pendingOperation?.fingerprint !== fingerprint) pendingOperation = { fingerprint, key: idempotencyKey() }
    const requestKey = pendingOperation.key
    try {
      const result = await createCreditAdminOperation({
        headers: { 'Idempotency-Key': requestKey },
        body,
      })
      if (result.error) throw result.error
      pendingOperation = null
      await load({ clearOutcome: false })
      outcome.value = {
        kind: 'success',
        diagnostic: makeDiagnostic('CREDITS_ADMIN_OPERATION_SUCCEEDED', `${creditActionLabel(input.action)}已入账，账户和账目已刷新。`, false),
      }
      return true
    } catch (error) {
      outcome.value = {
        kind: 'error',
        diagnostic: diagnosticFromError(error, 'CREDITS_ADMIN_OPERATION_FAILED', `${creditActionLabel(input.action)}没有完成。重新提交同一操作不会重复入账。`),
      }
      return false
    } finally {
      acting.value = null
    }
  }

  watch(projectId, () => {
    void load()
  }, { immediate: true })

  return {
    account,
    ledger,
    holds,
    ledgerPage,
    holdsPage,
    pageSize,
    acting,
    outcome,
    load,
    loadLedger,
    loadHolds,
    submitAdminOperation,
  }
}

export type { CreditAccountSchema, CreditHold, CreditLedgerEntry }
