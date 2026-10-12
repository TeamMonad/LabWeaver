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
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey } from '@/utils/format'

function diagnostic(error: unknown, fallbackCode: string, fallbackMessage: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackMessage, problem?.retryable ?? true)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

const fixedDecimalPattern = /^-?(0|[1-9][0-9]*)\.[0-9]{6}$/

export function isCreditsFixedDecimal(value: string): boolean {
  return fixedDecimalPattern.test(value)
}

export function isPositiveCreditsAmount(value: string): boolean {
  return /^(0|[1-9][0-9]*)\.[0-9]{6}$/.test(value) && value !== '0.000000'
}

function asAccount(value: unknown): CreditAccountSchema {
  if (
    !isRecord(value)
    || typeof value.projectId !== 'string'
    || typeof value.currency !== 'string'
    || typeof value.balance !== 'string'
    || typeof value.available !== 'string'
    || typeof value.reserved !== 'string'
    || typeof value.spent !== 'string'
    || typeof value.pending !== 'string'
    || typeof value.unknown !== 'string'
    || typeof value.platformAbsorbed !== 'string'
    || typeof value.unknownCount !== 'number'
    || !Number.isSafeInteger(value.unknownCount)
    || typeof value.revision !== 'number'
    || !Number.isSafeInteger(value.revision)
  ) {
    throw new Error('Credits 账户返回了无法识别的数据。')
  }
  return value as CreditAccountSchema
}

function asLedgerPage(value: unknown): CreditLedgerPageSchema {
  if (
    !isRecord(value)
    || !Array.isArray(value.items)
    || typeof value.page !== 'number'
    || !Number.isSafeInteger(value.page)
    || value.page < 1
    || typeof value.pageSize !== 'number'
    || !Number.isSafeInteger(value.pageSize)
    || value.pageSize < 1
    || typeof value.hasMore !== 'boolean'
  ) {
    throw new Error('Credits 账目返回了无法识别的数据。')
  }
  for (const item of value.items) {
    if (
      !isRecord(item)
      || typeof item.id !== 'string'
      || typeof item.kind !== 'string'
      || typeof item.operationId !== 'string'
      || typeof item.reason !== 'string'
      || typeof item.balanceDelta !== 'string'
      || typeof item.reservedDelta !== 'string'
      || typeof item.spentDelta !== 'string'
      || typeof item.platformAbsorbed !== 'string'
      || typeof item.createdAt !== 'string'
    ) {
      throw new Error('Credits 账目条目返回了无法识别的数据。')
    }
  }
  return value as CreditLedgerPageSchema
}

function asHoldPage(value: unknown): CreditHoldPageSchema {
  if (
    !isRecord(value)
    || !Array.isArray(value.items)
    || typeof value.page !== 'number'
    || !Number.isSafeInteger(value.page)
    || value.page < 1
    || typeof value.pageSize !== 'number'
    || !Number.isSafeInteger(value.pageSize)
    || value.pageSize < 1
    || typeof value.hasMore !== 'boolean'
  ) {
    throw new Error('Credits 预占返回了无法识别的数据。')
  }
  for (const item of value.items) {
    if (
      !isRecord(item)
      || typeof item.id !== 'string'
      || typeof item.projectId !== 'string'
      || typeof item.requestId !== 'string'
      || typeof item.businessKey !== 'string'
      || typeof item.ownerClientId !== 'string'
      || typeof item.amount !== 'string'
      || typeof item.remaining !== 'string'
      || typeof item.state !== 'string'
      || typeof item.measurementUnknown !== 'boolean'
      || typeof item.createdAt !== 'string'
    ) {
      throw new Error('Credits 预占条目返回了无法识别的数据。')
    }
  }
  return value as CreditHoldPageSchema
}

export type CreditAdminAction = CreditAdminOperationSchema['action']

export interface CreditAdminFormInput {
  action: CreditAdminAction
  amount: string
  reason: string
  destinationProjectId: string
  entryId: string
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
      const pageData = asLedgerPage(result.data)
      ledger.value = pageData.items.length || pageData.page > 1
        ? { kind: 'success', data: pageData }
        : { kind: 'empty' }
      ledgerPage.value = pageData.page
    } catch (error) {
      if (!current(id, generation)) return
      ledger.value = {
        kind: 'error',
        diagnostic: diagnostic(error, 'CREDITS_LEDGER_LOAD_FAILED', '无法加载 Credits 账目。'),
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
      const pageData = asHoldPage(result.data)
      holds.value = pageData.items.length || pageData.page > 1
        ? { kind: 'success', data: pageData }
        : { kind: 'empty' }
      holdsPage.value = pageData.page
    } catch (error) {
      if (!current(id, generation)) return
      holds.value = {
        kind: 'error',
        diagnostic: diagnostic(error, 'CREDITS_HOLDS_LOAD_FAILED', '无法加载 Credits 预占。'),
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
      account.value = { kind: 'success', data: asAccount(result.data) }
    } catch (error) {
      if (!current(id, generation)) return
      account.value = {
        kind: 'error',
        diagnostic: diagnostic(error, 'CREDITS_ACCOUNT_LOAD_FAILED', '无法加载 Credits 账户。'),
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
    const requestKey = idempotencyKey()
    try {
      const result = await createCreditAdminOperation({
        headers: { 'Idempotency-Key': requestKey },
        body,
      })
      if (result.error) throw result.error
      await load({ clearOutcome: false })
      outcome.value = {
        kind: 'success',
        diagnostic: makeDiagnostic('CREDITS_ADMIN_OPERATION_SUCCEEDED', 'Credits 管理员操作已提交。', false),
      }
      return true
    } catch (error) {
      outcome.value = {
        kind: 'error',
        diagnostic: diagnostic(error, 'CREDITS_ADMIN_OPERATION_FAILED', 'Credits 管理员操作失败。'),
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
