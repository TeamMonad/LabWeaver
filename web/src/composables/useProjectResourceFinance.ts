import { onScopeDispose, reactive, ref, watch, type Ref } from 'vue'
import { apiClient } from '@/api/client'
import { listProjectResourceLeases, listProjectResourceRequests, listProjectResourceUsage } from '@/generated/contracts'
import type {
  EnvironmentSummary,
  ResourceLeaseSchema,
  ResourceRequestSchema,
  ResourceUsagePageSchema,
  ResourceUsageRecord,
} from '@/generated/contracts'
import { fetchProjectEnvironments } from '@/composables/useProjectWorkEnvironments'
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

export interface ResourceUsageContext {
  environments: EnvironmentSummary[]
  requests: ResourceRequestSchema[]
  leases: ResourceLeaseSchema[]
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
  // The generated Axios transport exposes non-JSON error bodies through the
  // result.error field as the body value itself. Resource currently returns
  // its stable diagnostic code as a plain-text 404, so this branch must run
  // before ProblemDetails extraction instead of treating an unconfigured
  // budget as a load failure.
  if (error === 'LW_RESOURCE_BUDGET_NOT_FOUND') return true
  const problem = extractProblemDetails(error)
  if (problem?.diagnosticCode === 'LW_RESOURCE_BUDGET_NOT_FOUND') return true
  if (!isRecord(error)) return false
  const response = isRecord(error.response) ? error.response : undefined
  const responseData = response?.data
  return (
    responseData === 'LW_RESOURCE_BUDGET_NOT_FOUND' ||
    (isRecord(responseData) && responseData.diagnosticCode === 'LW_RESOURCE_BUDGET_NOT_FOUND')
  )
}

function isFiniteNonNegativeNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0
}

function isUsageRecord(value: unknown): value is ResourceUsageRecord {
  if (!isRecord(value) || typeof value.id !== 'string' || typeof value.projectId !== 'string' || typeof value.sourceEventId !== 'string') return false
  if (!['compute', 'storage'].includes(String(value.kind)) || !['pending', 'settled', 'unsettled'].includes(String(value.settlement))) return false
  if (typeof value.measuredFrom !== 'string' || typeof value.measuredUntil !== 'string' || typeof value.observedAt !== 'string') return false
  if (!isRecord(value.measurement) || (value.measurement.state !== 'known' && value.measurement.state !== 'unknown')) return false
  if (value.measurement.state === 'unknown') {
    if (typeof value.measurement.reason !== 'string' || !value.measurement.reason.trim()) return false
  } else if (!isRecord(value.measurement.quantities)
    || !isFiniteNonNegativeNumber(value.measurement.quantities.cpuMillicoreSeconds)
    || !isFiniteNonNegativeNumber(value.measurement.quantities.gpuUnitSeconds)
    || !isFiniteNonNegativeNumber(value.measurement.quantities.memoryByteSeconds)
    || !isFiniteNonNegativeNumber(value.measurement.quantities.storageByteSeconds)) {
    return false
  }
  if (!isRecord(value.target) || typeof value.target.kind !== 'string') return false
  if (value.target.kind === 'experiment_environment') return typeof value.target.environmentId === 'string'
  return value.target.kind === 'resource_request'
    && typeof value.target.requestId === 'string'
    && (value.target.leaseId === undefined || value.target.leaseId === null || typeof value.target.leaseId === 'string')
}

function asUsagePage(value: unknown): ResourceUsagePageSchema {
  if (!isRecord(value)
    || !Array.isArray(value.items)
    || !value.items.every(isUsageRecord)
    || typeof value.page !== 'number'
    || !Number.isSafeInteger(value.page)
    || value.page < 1
    || typeof value.pageSize !== 'number'
    || !Number.isSafeInteger(value.pageSize)
    || value.pageSize < 1
    || typeof value.hasMore !== 'boolean') {
    throw new Error('Resource usage response is not a valid page')
  }
  return value as ResourceUsagePageSchema
}

type UntypedApiResult = { data?: unknown; error?: unknown }

async function getFinanceResource(url: string): Promise<UntypedApiResult> {
  try {
    return await apiClient.get<unknown, unknown>({ url })
  } catch (error) {
    return { error }
  }
}

async function getUsagePage(projectId: string, page: number, pageSize: number): Promise<UntypedApiResult> {
  try {
    return await listProjectResourceUsage({
      path: { projectId },
      query: { page, pageSize },
    })
  } catch (error) {
    return { error }
  }
}

function listData(result: unknown, message: string): unknown[] {
  if (!isRecord(result)) throw new Error(message)
  if (result.error) throw result.error
  if (!Array.isArray(result.data)) throw new Error(message)
  return result.data
}

function environmentData(result: unknown): EnvironmentSummary[] {
  if (!Array.isArray(result)) throw new Error('项目环境列表返回了无法识别的数据。')
  return result as EnvironmentSummary[]
}

function requestData(result: unknown): ResourceRequestSchema[] {
  return listData(result, '资源申请列表返回了无法识别的数据。') as ResourceRequestSchema[]
}

function leaseData(result: unknown): ResourceLeaseSchema[] {
  return listData(result, '资源租约列表返回了无法识别的数据。') as ResourceLeaseSchema[]
}

export function useProjectResourceFinance(projectId: Ref<string | null>) {
  const budget = ref<AsyncState<ResourceBudget>>({ kind: 'idle' })
  const charges = ref<AsyncState<ResourceCharge[]>>({ kind: 'idle' })
  const usage = ref<AsyncState<ResourceUsagePageSchema>>({ kind: 'idle' })
  const usageContext = ref<AsyncState<ResourceUsageContext>>({ kind: 'idle' })
  const usagePage = ref(1)
  const usagePageSize = ref(25)
  const usageRecordsById = ref<Record<string, ResourceUsageRecord>>({})
  const acting = ref<string | null>(null)
  const outcome = ref<{ kind: 'success' | 'error'; diagnostic: DiagnosticViewModel } | null>(null)
  let loadGeneration = 0
  let usageGeneration = 0

  function usageRequestCurrent(id: string, generation: number, requestGeneration: number) {
    return projectId.value === id && generation === loadGeneration && requestGeneration === usageGeneration
  }

  async function loadUsageContext(id: string, records: ResourceUsageRecord[], generation: number, requestGeneration: number) {
    if (records.length === 0) {
      if (usageRequestCurrent(id, generation, requestGeneration)) usageContext.value = { kind: 'success', data: { environments: [], requests: [], leases: [] } }
      return
    }
    const needsRequestContext = records.some((record) => record.target.kind === 'resource_request')
    usageContext.value = { kind: 'loading', message: '加载用量关联信息…' }
    const [environmentsResult, requestsResult, leasesResult] = await Promise.allSettled([
      fetchProjectEnvironments({ projectId: id, limit: 100 }),
      needsRequestContext ? listProjectResourceRequests({ path: { projectId: id } }) : Promise.resolve({ data: [] }),
      needsRequestContext ? listProjectResourceLeases({ path: { projectId: id } }) : Promise.resolve({ data: [] }),
    ])
    if (!usageRequestCurrent(id, generation, requestGeneration)) return
    if (environmentsResult.status === 'rejected' || requestsResult.status === 'rejected' || leasesResult.status === 'rejected') {
      usageContext.value = {
        kind: 'error',
        diagnostic: makeDiagnostic('RESOURCE_USAGE_CONTEXT_LOAD_FAILED', '项目用量已加载，但关联环境或资源申请信息暂时不可用；名称会在刷新后重试。', true),
      }
      return
    }
    try {
      usageContext.value = {
        kind: 'success',
        data: {
          environments: environmentData(environmentsResult.value),
          requests: requestData(requestsResult.value),
          leases: leaseData(leasesResult.value),
        },
      }
    } catch (error) {
      usageContext.value = {
        kind: 'error',
        diagnostic: diagnostic(error, 'RESOURCE_USAGE_CONTEXT_INVALID', '项目用量关联信息暂时无法识别；名称会在刷新后重试。'),
      }
    }
  }

  async function applyUsageResult(id: string, result: UntypedApiResult, generation: number, requestGeneration: number, resetCache: boolean) {
    if (!usageRequestCurrent(id, generation, requestGeneration)) return
    if (result.error) {
      usage.value = { kind: 'error', diagnostic: diagnostic(result.error, 'RESOURCE_USAGE_LOAD_FAILED', '加载项目用量失败') }
      usageContext.value = { kind: 'idle' }
      return
    }
    let data: ResourceUsagePageSchema
    try {
      data = asUsagePage(result.data)
      if (data.items.some((record) => record.projectId !== id)) throw new Error('Resource usage response contains another project')
    } catch (error) {
      usage.value = { kind: 'error', diagnostic: diagnostic(error, 'RESOURCE_USAGE_INVALID', 'Resource 返回了无法识别的项目用量。') }
      usageContext.value = { kind: 'idle' }
      return
    }
    if (!usageRequestCurrent(id, generation, requestGeneration)) return
    usagePage.value = data.page
    usagePageSize.value = data.pageSize
    usage.value = { kind: 'success', data }
    if (resetCache) usageRecordsById.value = {}
    usageRecordsById.value = {
      ...usageRecordsById.value,
      ...Object.fromEntries(data.items.map((record) => [record.id, record])),
    }
    await loadUsageContext(id, data.items, generation, requestGeneration)
  }

  async function loadUsagePage(page = 1) {
    const id = projectId.value
    const generation = loadGeneration
    const requestGeneration = ++usageGeneration
    const requestedPage = Number.isSafeInteger(page) && page >= 1 ? page : 1
    if (!id) {
      usage.value = { kind: 'idle' }
      usageContext.value = { kind: 'idle' }
      usagePage.value = 1
      usageRecordsById.value = {}
      return
    }
    usage.value = { kind: 'loading', message: '加载项目用量…' }
    usageContext.value = { kind: 'idle' }
    await applyUsageResult(id, await getUsagePage(id, requestedPage, usagePageSize.value), generation, requestGeneration, requestedPage === 1)
  }

  async function load() {
    const id = projectId.value
    const generation = ++loadGeneration
    const requestGeneration = ++usageGeneration
    outcome.value = null
    if (!id) {
      budget.value = { kind: 'idle' }
      charges.value = { kind: 'idle' }
      usage.value = { kind: 'idle' }
      usageContext.value = { kind: 'idle' }
      usagePage.value = 1
      usageRecordsById.value = {}
      return
    }
    budget.value = { kind: 'loading', message: '加载项目预算…' }
    charges.value = { kind: 'loading', message: '加载费用明细…' }
    usage.value = { kind: 'loading', message: '加载项目用量…' }
    usageContext.value = { kind: 'idle' }
    usagePage.value = 1
    usageRecordsById.value = {}
    const [budgetResult, chargesResult, usageResult] = await Promise.all([
      getFinanceResource(`/api/v1/projects/${encodeURIComponent(id)}/resource-budget`),
      getFinanceResource(`/api/v1/projects/${encodeURIComponent(id)}/charges`),
      getUsagePage(id, 1, usagePageSize.value),
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
    await applyUsageResult(id, usageResult, generation, requestGeneration, true)
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
  onScopeDispose(() => { loadGeneration += 1; usageGeneration += 1 })

  return reactive({ budget, charges, usage, usageContext, usagePage, usagePageSize, usageRecordsById, acting, outcome, load, loadUsagePage, saveBudget, adjust })
}
