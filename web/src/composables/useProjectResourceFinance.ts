import { onScopeDispose, reactive, ref, watch, type Ref } from 'vue'
import { apiClient } from '@/api/client'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

export interface ResourceMoney {
  currency: string
  amount: string
}

export interface ResourceBudget {
  id: string
  projectId: string
  courseId?: string | null
  limit: ResourceMoney
  warningAt: ResourceMoney
  spent: ResourceMoney
  revision: number
  updatedAt: string
}

export interface ResourceChargeLine {
  rateId: string
  rateRevision: number
  unit: string
  quantity: number
  unitQuantity: number
  unitPrice: ResourceMoney
  amount: ResourceMoney
}

export interface ResourceCharge {
  id: string
  usageRecordId: string
  projectId: string
  courseId?: string | null
  lines: ResourceChargeLine[]
  total: ResourceMoney
  settlement: 'pending' | 'settled' | 'unsettled'
  createdAt: string
  adjustmentOf?: string | null
  adjustmentReason?: string | null
  adjustedBy?: string | null
  diagnosticCode?: string | null
}

export interface ResourceBudgetInput {
  projectId: string
  courseId?: string | null
  limit: ResourceMoney
  warningAt: ResourceMoney
}

export interface ResourceAdjustmentInput {
  amount: ResourceMoney
  reason: string
}

function diagnostic(error: unknown, fallbackCode: string, fallbackMessage: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackMessage, problem?.retryable ?? true)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

function isMoney(value: unknown): value is ResourceMoney {
  return isRecord(value) && typeof value.currency === 'string' && typeof value.amount === 'string'
}

function isBudget(value: unknown): value is ResourceBudget {
  return (
    isRecord(value) &&
    typeof value.id === 'string' &&
    typeof value.projectId === 'string' &&
    (value.courseId === null || value.courseId === undefined || typeof value.courseId === 'string') &&
    isMoney(value.limit) &&
    isMoney(value.warningAt) &&
    isMoney(value.spent) &&
    Number.isSafeInteger(value.revision) &&
    typeof value.updatedAt === 'string'
  )
}

function isChargeLine(value: unknown): value is ResourceChargeLine {
  return (
    isRecord(value) &&
    typeof value.rateId === 'string' &&
    Number.isSafeInteger(value.rateRevision) &&
    typeof value.unit === 'string' &&
    Number.isSafeInteger(value.quantity) &&
    Number.isSafeInteger(value.unitQuantity) &&
    isMoney(value.unitPrice) &&
    isMoney(value.amount)
  )
}

function isCharge(value: unknown): value is ResourceCharge {
  return (
    isRecord(value) &&
    typeof value.id === 'string' &&
    typeof value.usageRecordId === 'string' &&
    typeof value.projectId === 'string' &&
    (value.courseId === null || value.courseId === undefined || typeof value.courseId === 'string') &&
    Array.isArray(value.lines) &&
    value.lines.every(isChargeLine) &&
    isMoney(value.total) &&
    ['pending', 'settled', 'unsettled'].includes(String(value.settlement)) &&
    typeof value.createdAt === 'string' &&
    (value.adjustmentOf === null || value.adjustmentOf === undefined || typeof value.adjustmentOf === 'string') &&
    (value.adjustmentReason === null || value.adjustmentReason === undefined || typeof value.adjustmentReason === 'string') &&
    (value.adjustedBy === null || value.adjustedBy === undefined || typeof value.adjustedBy === 'string') &&
    (value.diagnosticCode === null || value.diagnosticCode === undefined || typeof value.diagnosticCode === 'string')
  )
}

function asBudget(value: unknown): ResourceBudget {
  if (!isBudget(value)) throw new Error('Resource budget response is not a valid budget')
  return value
}

function asCharges(value: unknown): ResourceCharge[] {
  if (!Array.isArray(value) || !value.every(isCharge)) throw new Error('Resource charges response is not valid')
  return value
}

function isBudgetNotFound(error: unknown): boolean {
  const problem = extractProblemDetails(error)
  if (problem?.diagnosticCode === 'LW_RESOURCE_BUDGET_NOT_FOUND') return true
  if (!isRecord(error)) return false
  const response = isRecord(error.response) ? error.response : undefined
  const responseData = response?.data
  return (
    error.status === 404 ||
    response?.status === 404 ||
    responseData === 'LW_RESOURCE_BUDGET_NOT_FOUND' ||
    (isRecord(responseData) && responseData.diagnosticCode === 'LW_RESOURCE_BUDGET_NOT_FOUND')
  )
}

export function useProjectResourceFinance(projectId: Ref<string | null>) {
  const budget = ref<AsyncState<ResourceBudget>>({ kind: 'idle' })
  const charges = ref<AsyncState<ResourceCharge[]>>({ kind: 'idle' })
  const acting = ref<string | null>(null)
  const outcome = ref<{ kind: 'success' | 'error'; diagnostic: DiagnosticViewModel } | null>(null)
  let loadGeneration = 0

  async function load() {
    const id = projectId.value
    const generation = ++loadGeneration
    outcome.value = null
    if (!id) {
      budget.value = { kind: 'idle' }
      charges.value = { kind: 'idle' }
      return
    }
    budget.value = { kind: 'loading', message: '加载项目预算…' }
    charges.value = { kind: 'loading', message: '加载费用明细…' }
    const [budgetResult, chargesResult] = await Promise.all([
      apiClient.get<unknown, unknown>({ url: `/api/v1/projects/${encodeURIComponent(id)}/resource-budget` }),
      apiClient.get<unknown, unknown>({ url: `/api/v1/projects/${encodeURIComponent(id)}/charges` }),
    ])
    if (generation !== loadGeneration) return
    if (budgetResult.error && isBudgetNotFound(budgetResult.error)) {
      budget.value = { kind: 'empty' }
    } else if (budgetResult.error) {
      budget.value = { kind: 'error', diagnostic: diagnostic(budgetResult.error, 'RESOURCE_BUDGET_LOAD_FAILED', '加载项目预算失败') }
    } else {
      try {
        budget.value = { kind: 'success', data: asBudget(budgetResult.data) }
      } catch (error) {
        budget.value = { kind: 'error', diagnostic: diagnostic(error, 'RESOURCE_BUDGET_INVALID', 'Resource 返回了无法识别的预算。') }
      }
    }
    if (chargesResult.error) {
      charges.value = { kind: 'error', diagnostic: diagnostic(chargesResult.error, 'RESOURCE_CHARGES_LOAD_FAILED', '加载费用明细失败') }
    } else {
      try {
        const items = asCharges(chargesResult.data)
        charges.value = items.length > 0 ? { kind: 'success', data: items } : { kind: 'empty' }
      } catch (error) {
        charges.value = { kind: 'error', diagnostic: diagnostic(error, 'RESOURCE_CHARGES_INVALID', 'Resource 返回了无法识别的费用明细。') }
      }
    }
  }

  async function saveBudget(input: ResourceBudgetInput): Promise<boolean> {
    const current = budget.value.kind === 'success' ? budget.value.data : null
    if (acting.value || (current && current.projectId !== input.projectId)) return false
    if (!input.limit.amount.match(/^(0|[1-9][0-9]*)\.[0-9]{6}$/) || !input.warningAt.amount.match(/^(0|[1-9][0-9]*)\.[0-9]{6}$/)) {
      outcome.value = { kind: 'error', diagnostic: makeDiagnostic('RESOURCE_BUDGET_AMOUNT_INVALID', '预算金额必须使用六位小数。', false) }
      return false
    }
    acting.value = 'budget'
    outcome.value = null
    try {
      const result = await apiClient.put<ResourceBudget, unknown>({
        url: `/api/v1/projects/${encodeURIComponent(input.projectId)}/resource-budget`,
        headers: {
          'Idempotency-Key': idempotencyKey(),
          ...(current ? { 'If-Match': ifMatch(current.revision) } : {}),
        },
        body: input,
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: diagnostic(result.error, 'RESOURCE_BUDGET_SAVE_FAILED', '保存项目预算失败') }
        return false
      }
      outcome.value = { kind: 'success', diagnostic: makeDiagnostic('RESOURCE_BUDGET_SAVED', '项目预算已更新。', false) }
      await load()
      return true
    } finally {
      acting.value = null
    }
  }

  async function adjust(charge: ResourceCharge, input: ResourceAdjustmentInput): Promise<boolean> {
    const id = projectId.value
    if (!id || acting.value || charge.projectId !== id) return false
    if (!input.reason.trim() || !input.amount.amount.match(/^-?(0|[1-9][0-9]*)\.[0-9]{6}$/)) {
      outcome.value = { kind: 'error', diagnostic: makeDiagnostic('RESOURCE_ADJUSTMENT_INVALID', '调整金额必须使用六位小数且填写原因。', false) }
      return false
    }
    acting.value = `adjust:${charge.id}`
    outcome.value = null
    try {
      const result = await apiClient.post<ResourceCharge, unknown>({
        url: `/api/v1/projects/${encodeURIComponent(id)}/charges/${encodeURIComponent(charge.id)}/adjustments`,
        headers: { 'Idempotency-Key': idempotencyKey() },
        body: input,
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: diagnostic(result.error, 'RESOURCE_ADJUSTMENT_FAILED', '创建费用调整失败') }
        return false
      }
      outcome.value = { kind: 'success', diagnostic: makeDiagnostic('RESOURCE_ADJUSTMENT_CREATED', '费用调整已记录。', false) }
      await load()
      return true
    } finally {
      acting.value = null
    }
  }

  watch(projectId, () => void load(), { immediate: true })
  onScopeDispose(() => { loadGeneration += 1 })

  return reactive({ budget, charges, acting, outcome, load, saveBudget, adjust })
}
