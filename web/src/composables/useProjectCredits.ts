import { computed, onScopeDispose, ref, watch, type Ref } from 'vue'
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
import { clearPendingCommand, listPendingCommands, readPendingCommand, writePendingCommand } from '@/utils/pendingCommands'

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

/**
 * Answers that prove the server did not apply the command; any other failure may have been applied.
 * LW_CREDITS_SCOPE_DENIED is not one of them: authorization runs before the idempotency lookup, so a
 * denied replay says nothing about whether the original request was booked.
 */
const NOT_APPLIED_CODES = ['LW_CREDITS_INVALID', 'LW_CREDITS_PRECISION', 'LW_CREDITS_INSUFFICIENT', 'LW_CREDITS_NOT_FOUND']

const PENDING_SCOPE_PREFIX = 'credits-admin:'

export interface PendingCreditOperation {
  scope: string
  input: CreditAdminFormInput
  /** False when the browser could not store the retry record: it is lost on refresh. */
  persisted: boolean
}

export type CreditAdminSubmitResult = 'accepted' | 'failed' | 'stale'

const NOT_PERSISTED_NEXT_STEP = '浏览器无法保存这笔操作的重试记录：在本页点击“重新提交”仍会沿用原请求，但刷新或关闭页面后无法安全重试，请先在账目中确认是否已入账。'

/** Rows per ledger/holds page; the server accepts 1..100. */
export const CREDITS_PAGE_LIMIT = 25

/** Which page of a cursor list to show next; cursors are forward-only, so going back replays an earlier cursor. */
export type CreditsPageMove = 'current' | 'next' | 'previous'

/**
 * Project Credits account, ledger and holds, plus platform-admin writes.
 *
 * Every admin write without a definite outcome is kept as a pending command
 * (session storage, per actor) under its own scope: project + canonical body.
 * A retry, a refresh or resubmitting the same operation replays it with the
 * same Idempotency-Key, and a different draft or another project never
 * replaces it. The entry is removed only when that operation is accepted or
 * the server proves it was not applied. An answer that arrives after the
 * project or actor changed only updates storage, never the current view.
 */
export function useProjectCredits(projectId: Ref<string | null>, actorId: Ref<string | null>) {
  const account = ref<AsyncState<CreditAccountSchema>>({ kind: 'idle' })
  const ledger = ref<AsyncState<CreditLedgerPageSchema>>({ kind: 'idle' })
  const holds = ref<AsyncState<CreditHoldPageSchema>>({ kind: 'idle' })
  /** Cursors that reached the shown page; empty means page 1. The total page count is never known. */
  const ledgerTrail = ref<string[]>([])
  const holdsTrail = ref<string[]>([])
  const ledgerPage = computed(() => ledgerTrail.value.length + 1)
  const holdsPage = computed(() => holdsTrail.value.length + 1)
  const acting = ref<string | null>(null)
  const outcome = ref<{ kind: 'success' | 'error'; diagnostic: DiagnosticViewModel } | null>(null)
  /** Unconfirmed admin operations of this actor in this project, restored after a refresh. */
  const pendingOperations = ref<PendingCreditOperation[]>([])
  let loadGeneration = 0
  let ledgerRequest = 0
  let holdsRequest = 0
  /** Changes with the project or actor; a write answer for an older context only updates storage. */
  let context = 0
  let disposed = false

  onScopeDispose(() => {
    disposed = true
  })

  function current(id: string, generation: number) {
    return !disposed && projectId.value === id && generation === loadGeneration
  }

  function refreshPending() {
    const id = projectId.value
    const actor = actorId.value
    pendingOperations.value = id && actor
      ? listPendingCommands<CreditAdminFormInput>(actor, `${PENDING_SCOPE_PREFIX}${id}:`)
        .map(({ scope, command, persisted }) => ({ scope, input: command.data, persisted }))
      : []
  }

  /** The trail to request for a move, or null when that page does not exist. */
  function moveTrail(trail: string[], state: AsyncState<{ nextCursor?: string | null }>, move: CreditsPageMove): string[] | null {
    if (move === 'current') return trail
    if (move === 'previous') return trail.length ? trail.slice(0, -1) : null
    const nextCursor = state.kind === 'success' ? state.data.nextCursor : null
    return nextCursor ? [...trail, nextCursor] : null
  }

  async function loadLedgerPage(id: string, trail: string[], generation: number) {
    const request = ++ledgerRequest
    ledger.value = { kind: 'loading', message: '加载 Credits 账目…' }
    const cursor = trail[trail.length - 1]
    try {
      const result = await listProjectCreditLedger({
        path: { projectId: id },
        query: { limit: CREDITS_PAGE_LIMIT, ...(cursor ? { cursor } : {}) },
      })
      if (!current(id, generation) || request !== ledgerRequest) return
      if (result.error) throw result.error
      const pageData = result.data
      ledgerTrail.value = trail
      ledger.value = pageData.items.length || trail.length
        ? { kind: 'success', data: pageData }
        : { kind: 'empty' }
    } catch (error) {
      if (!current(id, generation) || request !== ledgerRequest) return
      ledgerTrail.value = trail
      ledger.value = {
        kind: 'error',
        diagnostic: diagnosticFromError(error, 'CREDITS_LEDGER_LOAD_FAILED', '无法加载 Credits 账目。'),
      }
    }
  }

  async function loadHoldsPage(id: string, trail: string[], generation: number) {
    const request = ++holdsRequest
    holds.value = { kind: 'loading', message: '加载 Credits 预占…' }
    const cursor = trail[trail.length - 1]
    try {
      const result = await listProjectCreditHolds({
        path: { projectId: id },
        query: { limit: CREDITS_PAGE_LIMIT, ...(cursor ? { cursor } : {}) },
      })
      if (!current(id, generation) || request !== holdsRequest) return
      if (result.error) throw result.error
      const pageData = result.data
      holdsTrail.value = trail
      holds.value = pageData.items.length || trail.length
        ? { kind: 'success', data: pageData }
        : { kind: 'empty' }
    } catch (error) {
      if (!current(id, generation) || request !== holdsRequest) return
      holdsTrail.value = trail
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
    // A cursor belongs to the project that issued it; every (re)load starts at page 1.
    ledgerTrail.value = []
    holdsTrail.value = []
    if (!id) {
      account.value = { kind: 'idle' }
      ledger.value = { kind: 'idle' }
      holds.value = { kind: 'idle' }
      return
    }
    account.value = { kind: 'loading', message: '加载 Credits 账户…' }
    // The previous page's nextCursor must not be followed against the reset trail.
    ledger.value = { kind: 'loading', message: '加载 Credits 账目…' }
    holds.value = { kind: 'loading', message: '加载 Credits 预占…' }
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
      loadLedgerPage(id, [], generation),
      loadHoldsPage(id, [], generation),
    ])
  }

  async function loadLedger(move: CreditsPageMove) {
    const id = projectId.value
    if (!id) return
    const trail = moveTrail(ledgerTrail.value, ledger.value, move)
    if (trail) await loadLedgerPage(id, trail, loadGeneration)
  }

  async function loadHolds(move: CreditsPageMove) {
    const id = projectId.value
    if (!id) return
    const trail = moveTrail(holdsTrail.value, holds.value, move)
    if (trail) await loadHoldsPage(id, trail, loadGeneration)
  }

  async function submitAdminOperation(input: CreditAdminFormInput): Promise<CreditAdminSubmitResult> {
    const id = projectId.value
    const actor = actorId.value
    if (!id || !actor || acting.value !== null) return 'failed'
    const reason = input.reason.trim()
    if (!reason) {
      outcome.value = {
        kind: 'error',
        diagnostic: makeDiagnostic('CREDITS_REASON_REQUIRED', '管理员操作必须填写原因。', false),
      }
      return 'failed'
    }

    let body: CreditAdminOperationSchema
    if (input.action === 'grant') {
      if (!isPositiveCreditsAmount(input.amount)) {
        outcome.value = {
          kind: 'error',
          diagnostic: makeDiagnostic('CREDITS_AMOUNT_INVALID', '发放金额必须是大于零的六位小数字符串。', false),
        }
        return 'failed'
      }
      body = { action: 'grant', projectId: id, amount: input.amount, reason }
    } else if (input.action === 'transfer') {
      if (!isPositiveCreditsAmount(input.amount)) {
        outcome.value = {
          kind: 'error',
          diagnostic: makeDiagnostic('CREDITS_AMOUNT_INVALID', '转移金额必须是大于零的六位小数字符串。', false),
        }
        return 'failed'
      }
      const destinationProjectId = input.destinationProjectId.trim()
      if (!destinationProjectId || destinationProjectId === id) {
        outcome.value = {
          kind: 'error',
          diagnostic: makeDiagnostic('CREDITS_DESTINATION_INVALID', '转移必须指定与当前项目不同的目标项目。', false),
        }
        return 'failed'
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
        return 'failed'
      }
      body = { action: 'adjust', projectId: id, amount: input.amount, reason }
    } else {
      const entryId = input.entryId.trim()
      if (!entryId) {
        outcome.value = {
          kind: 'error',
          diagnostic: makeDiagnostic('CREDITS_ENTRY_REQUIRED', '冲正必须指定原账目条目 ID。', false),
        }
        return 'failed'
      }
      body = { action: 'reverse', entryId, reason }
    }

    const scope = `${PENDING_SCOPE_PREFIX}${id}:${JSON.stringify(body)}`
    const command = readPendingCommand<CreditAdminFormInput>(actor, scope)
      ?? { fingerprint: scope, idempotencyKey: idempotencyKey(), data: { ...input, reason } }
    // Recorded before sending: a response lost after this point is replayed, never re-minted.
    // When storage refuses it, pendingCommands keeps it in page memory under the same owner rules.
    const persisted = writePendingCommand(actor, scope, command)
    const requestContext = context
    const isCurrent = () => !disposed && requestContext === context
    acting.value = input.action
    outcome.value = null
    refreshPending()
    let next: { kind: 'success' | 'error'; diagnostic: DiagnosticViewModel }
    try {
      const result = await createCreditAdminOperation({
        headers: { 'Idempotency-Key': command.idempotencyKey },
        body,
      })
      if (result.error) throw result.error
      clearPendingCommand(actor, scope, command.idempotencyKey)
      next = {
        kind: 'success',
        diagnostic: makeDiagnostic('CREDITS_ADMIN_OPERATION_SUCCEEDED', `${creditActionLabel(input.action)}已入账，账户和账目已刷新。`, false),
      }
    } catch (error) {
      const diagnostic = diagnosticFromError(error, 'CREDITS_ADMIN_OPERATION_FAILED', `${creditActionLabel(input.action)}的结果尚未确认。重新提交同一操作不会重复入账。`)
      if (NOT_APPLIED_CODES.includes(diagnostic.code)) clearPendingCommand(actor, scope, command.idempotencyKey)
      else if (!persisted) diagnostic.nextStep = NOT_PERSISTED_NEXT_STEP
      next = { kind: 'error', diagnostic }
    }
    if (!isCurrent()) return 'stale'
    refreshPending()
    // The submit lock stays until the account and ledger show the booked
    // result, so the same draft cannot be sent again under a fresh key.
    if (next.kind === 'success') await load({ clearOutcome: false })
    if (!isCurrent()) return 'stale'
    outcome.value = next
    acting.value = null
    return next.kind === 'success' ? 'accepted' : 'failed'
  }

  /** Replays one unconfirmed operation with its original body and Idempotency-Key. */
  async function retryPending(scope: string): Promise<CreditAdminSubmitResult> {
    const pending = pendingOperations.value.find((item) => item.scope === scope)
    return pending ? submitAdminOperation(pending.input) : 'failed'
  }

  watch([projectId, actorId], () => {
    context += 1
    acting.value = null
    refreshPending()
    void load()
  }, { immediate: true })

  return {
    account,
    ledger,
    holds,
    ledgerPage,
    holdsPage,
    acting,
    outcome,
    pendingOperations,
    load,
    loadLedger,
    loadHolds,
    submitAdminOperation,
    retryPending,
  }
}

export type { CreditAccountSchema, CreditHold, CreditLedgerEntry }
