import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect } from '@playwright/test'
import {
  AUTH_STATE,
  expectJson,
  navigateFromHomeByUi,
  pollJson,
  uuidv7,
} from './live.mjs'
import { deleteEnvironmentByUi, stopEnvironmentByUi } from './environment-lifecycle.mjs'
import { cancelProjectResourceRequestByUi, releaseProjectLeaseByUi } from './real-resource.mjs'

const DIGEST_PINNED_IMAGE = /^[^\s@]+@sha256:([0-9a-f]{64})$/i
const BILLING_UNITS = Object.freeze([
  'cpu_millicore_second',
  'memory_byte_second',
  'storage_byte_second',
])
// Acceptance demonstration prices only: USD 1 per core-hour / GiB-hour.
// Existing operator-selected prices remain authoritative; these are not market recommendations.
export const DEFAULT_RATE_INPUTS = Object.freeze([
  Object.freeze({ unit: 'cpu_millicore_second', unitQuantity: 1000 * 3600, amount: '1.000000', currency: 'USD' }),
  Object.freeze({ unit: 'memory_byte_second', unitQuantity: 1024 ** 3 * 3600, amount: '1.000000', currency: 'USD' }),
  Object.freeze({ unit: 'storage_byte_second', unitQuantity: 1024 ** 3 * 3600, amount: '1.000000', currency: 'USD' }),
])
const GPU_MODES = Object.freeze(['exclusive', 'container_time_slice', 'vm_vgpu'])
const FIXED_DECIMAL = /^(0|[1-9][0-9]*)\.[0-9]{6}$/
const GPU_CLASS = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/
const CURRENCY = /^[A-Za-z0-9_-]{1,32}$/
const VM_BINDING = /^[a-z0-9](?:[a-z0-9._-]{0,126}[a-z0-9])?$/
const VM_SOURCE_REGISTRY_DIGEST = /^docker:\/\/[^\s@]+@sha256:[0-9a-f]{64}$/i
const VM_DISK_FORMATS = Object.freeze(['qcow2', 'raw'])
const GIB = 1024 ** 3
const FIXED_DECIMAL_SCALE = 1_000_000n

function fixedDecimalScaled(value, diagnostic) {
  if (typeof value !== 'string' || !FIXED_DECIMAL.test(value)) {
    throw new Error(`${diagnostic}:AMOUNT_INVALID`)
  }
  const [whole, fraction] = value.split('.')
  return BigInt(whole) * FIXED_DECIMAL_SCALE + BigInt(fraction)
}

function roundedScaledAmount(unitPrice, quantity, unitQuantity, diagnostic) {
  if (!Number.isSafeInteger(quantity) || quantity < 0 || !Number.isSafeInteger(unitQuantity) || unitQuantity < 1) {
    throw new Error(`${diagnostic}:QUANTITY_INVALID`)
  }
  const product = fixedDecimalScaled(unitPrice, diagnostic) * BigInt(quantity)
  const negative = product < 0n
  const magnitude = product < 0n ? -product : product
  const denominator = BigInt(unitQuantity)
  const quotient = magnitude / denominator
  const remainder = magnitude % denominator
  const roundUp = remainder * 2n > denominator
    || (remainder * 2n === denominator && quotient % 2n === 1n)
  const rounded = quotient + (roundUp ? 1n : 0n)
  return negative ? -rounded : rounded
}

function usageHasPositiveQuantity(usage) {
  if (usage.measurement?.state !== 'known') return false
  const quantities = usage.measurement.quantities
  const quantityFields = [
    'cpuMillicoreSeconds',
    'memoryByteSeconds',
    'storageByteSeconds',
    'gpuUnitSeconds',
  ]
  if (
    !quantities
    || typeof quantities !== 'object'
    || quantityFields.some((field) => !Number.isSafeInteger(quantities[field]) || quantities[field] < 0)
    || (usage.kind === 'compute' && quantities.storageByteSeconds !== 0)
    || (usage.kind === 'storage' && (
      quantities.cpuMillicoreSeconds !== 0
      || quantities.memoryByteSeconds !== 0
      || quantities.gpuUnitSeconds !== 0
    ))
  ) return false
  if (usage.kind === 'compute') {
    return [quantities.cpuMillicoreSeconds, quantities.memoryByteSeconds, quantities.gpuUnitSeconds]
      .some((value) => Number.isSafeInteger(value) && value > 0)
  }
  if (usage.kind === 'storage') return Number.isSafeInteger(quantities.storageByteSeconds) && quantities.storageByteSeconds > 0
  return false
}

function usageHasValidInterval(usage) {
  if (typeof usage?.measuredFrom !== 'string' || typeof usage.measuredUntil !== 'string') return false
  const measuredFrom = Date.parse(usage.measuredFrom)
  const measuredUntil = Date.parse(usage.measuredUntil)
  return Number.isFinite(measuredFrom) && Number.isFinite(measuredUntil) && measuredUntil > measuredFrom
}

function assertGpuRateExpectation(gpu) {
  const rate = gpu.rate
  if (
    typeof gpu.class !== 'string'
    || gpu.class === ''
    || typeof gpu.mode !== 'string'
    || gpu.mode === ''
    || !rate
    || typeof rate.id !== 'string'
    || rate.id === ''
    || !Number.isSafeInteger(rate.revision)
    || rate.revision < 1
    || rate.gpuClass !== gpu.class
    || rate.gpuMode !== gpu.mode
    || rate.unit !== 'gpu_unit_second'
    || !Number.isSafeInteger(rate.unitQuantity)
    || rate.unitQuantity < 1
    || typeof rate.unitPrice?.currency !== 'string'
    || rate.unitPrice.currency === ''
    || !FIXED_DECIMAL.test(rate.unitPrice.amount ?? '')
  ) {
    throw new Error('LW_WORK_GPU_RATE_EXPECTATION_INVALID')
  }
  return rate
}

function lineHasValidAmount(line, diagnostic = 'LW_WORK_USAGE_CHARGE') {
  return fixedDecimalScaled(line.amount?.amount, diagnostic) >= 0n
}

function sumMatchingLineQuantities(lines, matches) {
  let total = 0
  for (const line of lines) {
    if (!matches(line) || !Number.isSafeInteger(line.quantity) || line.quantity <= 0) continue
    if (!lineHasValidAmount(line)) continue
    total += line.quantity
    if (!Number.isSafeInteger(total)) return null
  }
  return total
}

function lineHasGpuRate(line, gpu, rates = null) {
  const rate = gpu.rate
    ? assertGpuRateExpectation(gpu)
    : rates?.find((candidate) => candidate?.id === line.rateId && candidate.revision === line.rateRevision)
  if (!rate) return false
  return (
    line.unit === 'gpu_unit_second'
    && (!gpu.rate || (line.rateId === rate.id && line.rateRevision === rate.revision))
    && line.unitQuantity === rate.unitQuantity
    && line.unitPrice?.currency === rate.unitPrice?.currency
    && line.unitPrice?.amount === rate.unitPrice?.amount
    && rate.gpuClass === gpu.class
    && rate.gpuMode === gpu.mode
  )
}

function rateLineMatchesUsage(line, usage, rates) {
  if (rates == null) return true
  const rate = rates.find((candidate) => (
    candidate?.id === line.rateId
    && candidate.revision === line.rateRevision
  ))
  if (!rate) return false
  const usageFrom = Date.parse(usage.measuredFrom)
  const usageUntil = Date.parse(usage.measuredUntil)
  const effectiveFrom = Date.parse(rate.effectiveFrom)
  const effectiveUntil = rate.effectiveUntil == null ? null : Date.parse(rate.effectiveUntil)
  return (
    rate.unit === line.unit
    && rate.unitQuantity === line.unitQuantity
    && rate.unitPrice?.currency === line.unitPrice?.currency
    && rate.unitPrice?.amount === line.unitPrice?.amount
    && Number.isFinite(effectiveFrom)
    && effectiveFrom < usageUntil
    && (effectiveUntil === null || (Number.isFinite(effectiveUntil) && effectiveUntil > usageFrom))
  )
}

function ratesCoverUsageWindow(lines, usage, unit, rates) {
  if (rates == null) return true
  const usageFrom = Date.parse(usage.measuredFrom)
  const usageUntil = Date.parse(usage.measuredUntil)
  const intervals = lines
    .filter((line) => line.unit === unit && line.quantity > 0)
    .map((line) => rates.find((candidate) => (
      candidate?.id === line.rateId
      && candidate.revision === line.rateRevision
    )))
    .filter(Boolean)
    .map((rate) => ({
      from: Math.max(usageFrom, Date.parse(rate.effectiveFrom)),
      until: Math.min(usageUntil, rate.effectiveUntil == null ? usageUntil : Date.parse(rate.effectiveUntil)),
    }))
    .filter(({ from, until }) => Number.isFinite(from) && Number.isFinite(until) && until > from)
    .sort((left, right) => left.from - right.from || left.until - right.until)
  let cursor = usageFrom
  for (const interval of intervals) {
    if (interval.from > cursor) return false
    if (interval.until > cursor) cursor = interval.until
  }
  return cursor === usageUntil
}

function chargeMatchesUsageQuantities(charge, usage, gpu, rates = null) {
  const quantities = usage.measurement.quantities
  const lines = Array.isArray(charge.lines) ? charge.lines : []
  const requirements = []
  if (usage.kind === 'compute') {
    if (quantities.cpuMillicoreSeconds > 0) {
      requirements.push({
        unit: 'cpu_millicore_second',
        quantity: quantities.cpuMillicoreSeconds,
      })
    }
    if (quantities.memoryByteSeconds > 0) {
      requirements.push({
        unit: 'memory_byte_second',
        quantity: quantities.memoryByteSeconds,
      })
    }
    if (quantities.gpuUnitSeconds > 0) {
      if (gpu?.rate) assertGpuRateExpectation(gpu)
      requirements.push({
        unit: 'gpu_unit_second',
        quantity: quantities.gpuUnitSeconds,
        matches: gpu ? (line) => lineHasGpuRate(line, gpu, rates) : undefined,
      })
    }
  } else if (usage.kind === 'storage' && quantities.storageByteSeconds > 0) {
    requirements.push({
      unit: 'storage_byte_second',
      quantity: quantities.storageByteSeconds,
    })
  }
  if (requirements.length === 0) return false
  const expectedUnits = new Set(requirements.map(({ unit }) => unit))
  let lineTotal = 0n
  for (const line of lines) {
    if (!Number.isSafeInteger(line.quantity) || line.quantity < 0) return false
    if (
      line.amount?.currency !== line.unitPrice?.currency
      || charge.total?.currency !== line.amount?.currency
      || !lineHasValidAmount(line)
      || !rateLineMatchesUsage(line, usage, rates)
    ) return false
    const actualAmount = fixedDecimalScaled(line.amount?.amount, 'LW_WORK_USAGE_CHARGE_LINE')
    if (actualAmount !== roundedScaledAmount(
      line.unitPrice?.amount,
      line.quantity,
      line.unitQuantity,
      'LW_WORK_USAGE_CHARGE_LINE',
    )) return false
    lineTotal += actualAmount
    if (line.quantity === 0) continue
    if (!expectedUnits.has(line.unit)) return false
    if (line.unit === 'gpu_unit_second' && gpu && !lineHasGpuRate(line, gpu, rates)) return false
  }
  if (fixedDecimalScaled(charge.total?.amount, 'LW_WORK_USAGE_CHARGE') !== lineTotal) return false
  if (rates != null && requirements.some(({ unit }) => !ratesCoverUsageWindow(lines, usage, unit, rates))) return false
  return requirements.every((requirement) => {
    const total = sumMatchingLineQuantities(lines, (line) => (
      line.unit === requirement.unit
      && (requirement.matches == null || requirement.matches(line))
    ))
    return total === requirement.quantity
  })
}

/**
 * Match authoritative usage records to their immutable charges for one Work lease.
 * A project charge without the exact request/lease usage identity is never accepted.
 * Returns null while either compute or storage usage is unknown, unsettled, or missing.
 */
export function selectSettledWorkUsageChargesForLease({
  usageRecords,
  charges,
  projectId,
  requestId,
  leaseId,
  baselineChargeIds = new Set(),
  gpu = null,
}) {
  if (!Array.isArray(usageRecords) || !Array.isArray(charges)) throw new Error('LW_WORK_USAGE_READ_INVALID')
  if (!baselineChargeIds || typeof baselineChargeIds.has !== 'function') throw new Error('LW_WORK_USAGE_BASELINE_INVALID')
  if ([projectId, requestId, leaseId].some((value) => typeof value !== 'string' || value === '')) {
    throw new Error('LW_WORK_USAGE_SCOPE_INVALID')
  }

  const scopedUsage = usageRecords.filter((usage) => (
    usage?.projectId === projectId
    && usage.target?.kind === 'resource_request'
    && usage.target.requestId === requestId
    && usage.target.leaseId === leaseId
  ))
  if (
    scopedUsage.length === 0
    || scopedUsage.some((usage) => !usageHasValidInterval(usage))
    || scopedUsage.some((usage) => usage.measurement?.state !== 'known' || usage.settlement !== 'settled')
  ) return null
  const readyUsage = scopedUsage.filter((usage) => {
    if (usage.measurement?.state !== 'known') throw new Error('LW_WORK_USAGE_MEASUREMENT_INVALID')
    if (!['compute', 'storage'].includes(usage.kind)) throw new Error('LW_WORK_USAGE_KIND_INVALID')
    if (typeof usage.id !== 'string' || usage.id === '') throw new Error('LW_WORK_USAGE_ID_INVALID')
    return usageHasPositiveQuantity(usage)
  })
  if (!readyUsage.some((usage) => usage.kind === 'compute') || !readyUsage.some((usage) => usage.kind === 'storage')) return null

  const matches = []
  for (const usage of readyUsage) {
    const charge = charges.find((candidate) => (
      candidate?.projectId === projectId
      && candidate.usageRecordId === usage.id
      && candidate.adjustmentOf == null
      && !baselineChargeIds.has(candidate.id)
    ))
    if (!charge || charge.settlement !== 'settled') continue
    if (typeof charge.id !== 'string' || charge.id === '' || typeof charge.usageRecordId !== 'string') {
      throw new Error('LW_WORK_USAGE_CHARGE_ID_INVALID')
    }
    if (fixedDecimalScaled(charge.total?.amount, 'LW_WORK_USAGE_CHARGE') < 0n) continue
    if (!chargeMatchesUsageQuantities(charge, usage, gpu)) continue
    matches.push({ usage, charge })
  }
  if (!matches.some(({ usage }) => usage.kind === 'compute') || !matches.some(({ usage }) => usage.kind === 'storage')) return null
  return matches
}

function experimentUsageHasPositiveQuantities(usage, gpu) {
  if (!usageHasPositiveQuantity(usage)) return false
  const quantities = usage.measurement.quantities
  if (usage.kind === 'compute') {
    return quantities.cpuMillicoreSeconds > 0
      && quantities.memoryByteSeconds > 0
      && (gpu ? quantities.gpuUnitSeconds > 0 : quantities.gpuUnitSeconds === 0)
  }
  return usage.kind === 'storage' && quantities.storageByteSeconds > 0
}

function describeExperimentUsageState(usageRecords, projectId, environmentId) {
  const scoped = usageRecords.filter((usage) => (
    usage?.projectId === projectId
    && usage.target?.kind === 'experiment_environment'
    && usage.target.environmentId === environmentId
  ))
  if (scoped.length === 0) return 'missing'
  return scoped.map((usage) => (
    `${usage.id ?? 'missing'}:${usage.measurement?.state ?? 'missing'}:${usage.settlement ?? 'missing'}`
  )).join(',')
}

/**
 * Match settled teaching-environment usage to its exact immutable charges.
 * Teaching meters point at the environment itself; Work meters continue to
 * use selectSettledWorkUsageChargesForLease and request/lease identity.
 */
export function selectSettledExperimentUsageCharges({
  usageRecords,
  charges,
  projectId,
  environmentId,
  baselineChargeIds = new Set(),
  gpu = null,
  rates = null,
}) {
  if (!Array.isArray(usageRecords) || !Array.isArray(charges)) throw new Error('LW_EXPERIMENT_USAGE_READ_INVALID')
  if (!baselineChargeIds || typeof baselineChargeIds.has !== 'function') throw new Error('LW_EXPERIMENT_USAGE_BASELINE_INVALID')
  if ([projectId, environmentId].some((value) => typeof value !== 'string' || value === '')) {
    throw new Error('LW_EXPERIMENT_USAGE_SCOPE_INVALID')
  }
  if (rates != null && !Array.isArray(rates)) throw new Error('LW_EXPERIMENT_USAGE_RATES_INVALID')

  const scopedUsage = usageRecords.filter((usage) => (
    usage?.projectId === projectId
    && usage.target?.kind === 'experiment_environment'
    && usage.target.environmentId === environmentId
  ))
  if (
    scopedUsage.length === 0
    || scopedUsage.some((usage) => !usageHasValidInterval(usage))
    || scopedUsage.some((usage) => usage.measurement?.state !== 'known' || usage.settlement !== 'settled')
  ) return null

  const usageIds = new Set()
  for (const usage of scopedUsage) {
    if (usage.measurement?.state !== 'known') throw new Error('LW_EXPERIMENT_USAGE_MEASUREMENT_INVALID')
    if (!['compute', 'storage'].includes(usage.kind)) throw new Error('LW_EXPERIMENT_USAGE_KIND_INVALID')
    if (typeof usage.id !== 'string' || usage.id === '') throw new Error('LW_EXPERIMENT_USAGE_ID_INVALID')
    if (usageIds.has(usage.id)) throw new Error('LW_EXPERIMENT_USAGE_DUPLICATE')
    usageIds.add(usage.id)
    if (!experimentUsageHasPositiveQuantities(usage, gpu)) return null
  }
  if (!scopedUsage.some((usage) => usage.kind === 'compute') || !scopedUsage.some((usage) => usage.kind === 'storage')) return null

  const matches = []
  const chargeIds = new Set()
  for (const usage of scopedUsage) {
    const candidates = charges.filter((candidate) => (
      candidate?.projectId === projectId
      && candidate.usageRecordId === usage.id
      && candidate.adjustmentOf == null
      && !baselineChargeIds.has(candidate.id)
    ))
    if (candidates.length > 1) throw new Error(`LW_EXPERIMENT_USAGE_CHARGE_DUPLICATE:${usage.id}`)
    const charge = candidates[0]
    if (!charge || charge.settlement !== 'settled') return null
    if (typeof charge.id !== 'string' || charge.id === '' || typeof charge.usageRecordId !== 'string') {
      throw new Error('LW_EXPERIMENT_USAGE_CHARGE_ID_INVALID')
    }
    if (chargeIds.has(charge.id)) throw new Error('LW_EXPERIMENT_USAGE_CHARGE_DUPLICATE')
    chargeIds.add(charge.id)
    if (fixedDecimalScaled(charge.total?.amount, 'LW_EXPERIMENT_USAGE_CHARGE') < 0n) return null
    if (!chargeMatchesUsageQuantities(charge, usage, gpu, rates)) return null
    matches.push({ usage, charge })
  }
  return matches
}

export async function readProjectUsagePage(request, projectId, { page = 1, pageSize = 100, diagnosticCode = 'REAL_WORK_USAGE_READ_FAILED' } = {}) {
  if (
    typeof projectId !== 'string'
    || projectId === ''
    || !Number.isSafeInteger(page)
    || page < 1
    || !Number.isSafeInteger(pageSize)
    || pageSize < 1
    || pageSize > 100
  ) {
    throw new Error('LW_WORK_USAGE_PAGE_ARGUMENT_INVALID')
  }
  const result = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/usage?page=${page}&pageSize=${pageSize}`),
    diagnosticCode,
  )
  if (
    !result
    || !Array.isArray(result.items)
    || result.page !== page
    || result.pageSize !== pageSize
    || typeof result.hasMore !== 'boolean'
  ) {
    throw new Error('LW_WORK_USAGE_PAGE_INVALID')
  }
  return result
}

export async function readProjectUsage(request, projectId, { pageSize = 100 } = {}) {
  const items = []
  let page = 1
  for (;;) {
    const result = await readProjectUsagePage(request, projectId, { page, pageSize })
    items.push(...result.items)
    if (!result.hasMore) return items
    page += 1
    if (page > 1000) throw new Error('LW_WORK_USAGE_PAGE_LIMIT_EXCEEDED')
  }
}

export async function readResourceRates(context, diagnosticCode = 'REAL_WORK_RATES_READ_FAILED') {
  return await readResourceRatesInternal(context, diagnosticCode)
}

export async function waitForSettledWorkUsageCharges(browser, baseURL, {
  projectId,
  leases,
  baselineChargeIds = new Set(),
  gpu = null,
} = {}) {
  if (
    typeof projectId !== 'string'
    || projectId === ''
    || !Array.isArray(leases)
    || leases.length === 0
    || leases.some((lease) => (
      typeof lease?.requestId !== 'string'
      || lease.requestId === ''
      || typeof lease.leaseId !== 'string'
      || lease.leaseId === ''
    ))
  ) {
    throw new Error('LW_WORK_USAGE_LEASES_INVALID')
  }
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  let latestUsage = null
  let latestCharges = null
  let latestMatches = null
  try {
    await expect.poll(
      async () => {
        latestUsage = await readProjectUsage(context.request, projectId)
        latestCharges = await expectJson(
          await context.request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/charges`),
          'REAL_WORK_CHARGES_READ_FAILED',
        )
        if (!Array.isArray(latestCharges)) throw new Error('LW_WORK_CHARGES_RESPONSE_INVALID')
        latestMatches = leases.map((lease) => selectSettledWorkUsageChargesForLease({
          usageRecords: latestUsage,
          charges: latestCharges,
          projectId,
          requestId: lease.requestId,
          leaseId: lease.leaseId,
          baselineChargeIds,
          gpu,
        }))
        return latestMatches.every(Boolean)
      },
      { timeout: 300_000, intervals: [1000, 2000, 5000] },
    ).toBe(true)
    return {
      usageRecords: latestUsage,
      charges: latestCharges,
      matches: latestMatches.flat(),
    }
  } finally {
    await context.close()
  }
}

export async function waitForSettledExperimentUsageCharges(browser, baseURL, {
  projectId,
  environmentId,
  baselineChargeIds = new Set(),
  gpu = null,
  rates = null,
} = {}) {
  if (
    typeof projectId !== 'string'
    || projectId === ''
    || typeof environmentId !== 'string'
    || environmentId === ''
  ) {
    throw new Error('LW_EXPERIMENT_USAGE_SCOPE_INVALID')
  }
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  let latestUsage = null
  let latestCharges = null
  let latestMatches = null
  let selectionError = null
  try {
    const effectiveRates = rates ?? await readResourceRates(context, 'LW_EXPERIMENT_RATES_READ_FAILED')
    await expect.poll(
      async () => {
        latestUsage = await readProjectUsage(context.request, projectId)
        latestCharges = await expectJson(
          await context.request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/charges`),
          'LW_EXPERIMENT_CHARGES_READ_FAILED',
        )
        if (!Array.isArray(latestCharges)) throw new Error('LW_EXPERIMENT_CHARGES_RESPONSE_INVALID')
        try {
          latestMatches = selectSettledExperimentUsageCharges({
            usageRecords: latestUsage,
            charges: latestCharges,
            projectId,
            environmentId,
            baselineChargeIds,
            gpu,
            rates: effectiveRates,
          })
        } catch (error) {
          selectionError = error
          throw error
        }
        return latestMatches != null
      },
      { timeout: 300_000, intervals: [1000, 2000, 5000] },
    ).toBe(true)
    return {
      usageRecords: latestUsage,
      charges: latestCharges,
      matches: latestMatches,
    }
  } catch (error) {
    if (selectionError) throw selectionError
    if (latestUsage) {
      throw new Error(
        `LW_EXPERIMENT_USAGE_SETTLEMENT_TIMEOUT:${describeExperimentUsageState(latestUsage, projectId, environmentId)}`,
        { cause: error },
      )
    }
    throw error
  } finally {
    await context.close()
  }
}

export function selectPendingWorkTaskResourceRequest(requests, {
  projectId,
  runId,
  trackKind,
  attemptNumber,
  studentActorId,
  ignoredRequestIds = new Set(),
}) {
  if (!Array.isArray(requests)) throw new Error('WORK_TASK_RESOURCE_REQUESTS_INVALID')
  if (!(ignoredRequestIds instanceof Set)) throw new Error('WORK_TASK_RESOURCE_IGNORED_REQUEST_IDS_INVALID')
  const compactRunId = runId.replaceAll('-', '').toLowerCase()
  const requestPrefix = `authoring-${compactRunId}-`
  const pending = []
  for (const request of requests) {
    if (typeof request.requestKey !== 'string' || !request.requestKey.startsWith(requestPrefix)) continue
    const identity = request.requestKey.match(
      /^authoring-([0-9a-f]{32})-(environment|evaluation|work_configuration)-([1-9][0-9]*)-([0-9a-f]{32})$/i,
    )
    if (!identity) throw new Error(`WORK_TASK_RESOURCE_REQUEST_SCOPE_INVALID:${request.id ?? 'missing'}`)
    if (identity[2] !== trackKind || Number(identity[3]) !== attemptNumber) continue
    const taskRunId = request.target?.taskRunId
    if (
      request.projectId !== projectId
      || identity[1].toLowerCase() !== compactRunId
      || request.requesterId !== studentActorId
      || request.target?.kind !== 'task'
      || typeof taskRunId !== 'string'
      || taskRunId.replaceAll('-', '').toLowerCase() !== identity[4].toLowerCase()
      || typeof request.id !== 'string'
      || !Number.isSafeInteger(request.requestedResources?.cpuMillicores)
      || request.requestedResources.cpuMillicores <= 0
      || !Number.isSafeInteger(request.requestedResources?.memoryBytes)
      || request.requestedResources.memoryBytes <= 0
      || !Number.isSafeInteger(request.requestedResources?.storageBytes)
      || request.requestedResources.storageBytes <= 0
      || request.requestedResources.gpu != null
      || !['reviewing', 'allocating', 'active', 'expiring', 'expired', 'rejected', 'cancelled'].includes(request.state)
    ) {
      throw new Error(`WORK_TASK_RESOURCE_REQUEST_SCOPE_INVALID:${request.id ?? 'missing'}`)
    }
    if (ignoredRequestIds.has(request.id)) continue
    if (['expired', 'rejected', 'cancelled'].includes(request.state)) continue
    pending.push(request)
  }
  if (pending.length > 1) throw new Error('WORK_TASK_RESOURCE_REQUEST_DUPLICATE')
  return pending[0] ?? null
}

function requireDigestPinnedImage(value) {
  const image = typeof value === 'string' ? value.trim() : ''
  const match = image.match(DIGEST_PINNED_IMAGE)
  if (!match) throw new Error('LABWEAVER_E2E_SECURITY_BASE_IMAGE_MUST_BE_DIGEST_PINNED')
  return { image, digest: `sha256:${match[1].toLowerCase()}` }
}

/**
 * Resolve the explicit opt-in settings for the real Work provider path.
 * Ordinary Playwright runs continue to use the existing fixture package.
 */
export function realWorkConfig({ virtualMachine = false } = {}) {
  // An explicit resume continues from immutable IDs supplied by the caller.
  // It must not require another provider/base-image configuration because it
  // deliberately skips the already-paid authoring path.
  const resumeConfigured = [
    process.env.LABWEAVER_E2E_RESUME_PROJECT_ID,
    process.env.LABWEAVER_E2E_RESUME_RUN_ID,
    process.env.LABWEAVER_E2E_RESUME_RELEASE_ID,
  ].some((value) => typeof value === 'string' && value.trim() !== '')
  if (resumeConfigured || virtualMachine) return null
  if (process.env.LABWEAVER_E2E_REAL_PROVIDER !== '1') return null
  const model = process.env.LABWEAVER_E2E_PROVIDER_MODEL?.trim() ?? ''
  if (!model || /\s/.test(model)) throw new Error('LABWEAVER_E2E_PROVIDER_MODEL_REQUIRED')
  const base = requireDigestPinnedImage(process.env.LABWEAVER_E2E_SECURITY_BASE_IMAGE)
  return Object.freeze({ model, goldenBaseImage: base.image, goldenBaseDigest: base.digest })
}

/**
 * Resolve an explicit continuation of a completed Work authoring run.
 *
 * All three IDs are required together. The continuation never lists or picks
 * a latest run/release; the validation helper below reads exactly these
 * project, AgentRun, and release identities before starting the resource and
 * Work journey.
 */
export function realWorkResumeConfig() {
  const projectId = process.env.LABWEAVER_E2E_RESUME_PROJECT_ID?.trim() ?? ''
  const runId = process.env.LABWEAVER_E2E_RESUME_RUN_ID?.trim() ?? ''
  const releaseId = process.env.LABWEAVER_E2E_RESUME_RELEASE_ID?.trim() ?? ''
  if (!projectId && !runId && !releaseId) return null
  if (!projectId || !runId || !releaseId) {
    throw new Error('LABWEAVER_E2E_RESUME_WORK_PROJECT_RUN_RELEASE_REQUIRED')
  }
  return Object.freeze({
    projectId,
    runId,
    releaseId,
    // The public ProblemPackage projection exposes immutable object identities;
    // callers supply the exact original markers and resume never invents them.
    seedMarker: process.env.LABWEAVER_E2E_RESUME_SEED_MARKER?.trim() || null,
    persistenceMarker: process.env.LABWEAVER_E2E_RESUME_PERSISTENCE_MARKER?.trim() || null,
  })
}

/**
 * Resolve the explicit GPU request for the Work resource leg.
 *
 * A normal Work run remains CPU-only unless all four GPU inputs are supplied.
 * The rate amount and currency are used only when the admin UI has to create
 * the matching GPU rate; the catalog class and mode are still selected from
 * the Resource page's published catalog options.
 */
export function realWorkGpuConfig() {
  const gpuClass = process.env.LABWEAVER_E2E_WORK_GPU_CLASS?.trim() ?? ''
  const gpuMode = process.env.LABWEAVER_E2E_WORK_GPU_MODE?.trim() ?? ''
  const rateAmount = process.env.LABWEAVER_E2E_WORK_GPU_RATE_AMOUNT?.trim() ?? ''
  const rateCurrency = process.env.LABWEAVER_E2E_WORK_GPU_RATE_CURRENCY?.trim() ?? ''
  const rateEffectiveUntil = process.env.LABWEAVER_E2E_WORK_GPU_RATE_EFFECTIVE_UNTIL?.trim() ?? ''
  const present = [gpuClass, gpuMode, rateAmount, rateCurrency].filter((value) => value !== '')
  if (present.length === 0) {
    if (rateEffectiveUntil) throw new Error('LABWEAVER_E2E_WORK_GPU_EFFECTIVE_UNTIL_WITHOUT_RATE')
    return null
  }
  if (present.length !== 4) throw new Error('LABWEAVER_E2E_WORK_GPU_FIELDS_INCOMPLETE')
  if (!GPU_CLASS.test(gpuClass)) throw new Error('LABWEAVER_E2E_WORK_GPU_CLASS_INVALID')
  if (!GPU_MODES.includes(gpuMode)) throw new Error('LABWEAVER_E2E_WORK_GPU_MODE_INVALID')
  if (!FIXED_DECIMAL.test(rateAmount) || Number(rateAmount) <= 0) {
    throw new Error('LABWEAVER_E2E_WORK_GPU_RATE_AMOUNT_INVALID')
  }
  if (!CURRENCY.test(rateCurrency)) throw new Error('LABWEAVER_E2E_WORK_GPU_RATE_CURRENCY_INVALID')
  if (rateEffectiveUntil) {
    const effectiveUntil = Date.parse(rateEffectiveUntil)
    if (!Number.isFinite(effectiveUntil) || effectiveUntil <= Date.now()) {
      throw new Error('LABWEAVER_E2E_WORK_GPU_RATE_EFFECTIVE_UNTIL_INVALID')
    }
  }
  return Object.freeze({
    class: gpuClass,
    mode: gpuMode,
    count: 1,
    rate: Object.freeze({
      unit: 'gpu_unit_second',
      unitQuantity: 1,
      amount: rateAmount,
      currency: rateCurrency,
      gpuClass,
      gpuMode,
      ...(rateEffectiveUntil ? { effectiveUntil: rateEffectiveUntil } : {}),
    }),
  })
}

/**
 * Resolve exact VM bindings from the deployment-reviewed imported-base
 * catalog. The same values seed fresh Work authoring and validate a resume;
 * they are never inferred from a container image setting.
 */
export function realWorkVmConfig() {
  const providerBinding = process.env.LABWEAVER_E2E_VM_PROVIDER_BINDING?.trim() ?? ''
  const storageClassBinding = process.env.LABWEAVER_E2E_VM_STORAGE_CLASS_BINDING?.trim() ?? ''
  const baseDiskBinding = process.env.LABWEAVER_E2E_VM_BASE_DISK_BINDING?.trim() ?? ''
  const sourceRegistryDigest = process.env.LABWEAVER_E2E_VM_BASE_DISK_SOURCE_REGISTRY_DIGEST?.trim() ?? ''
  const capacityBytes = process.env.LABWEAVER_E2E_VM_BASE_DISK_CAPACITY_BYTES?.trim() ?? ''
  const sshPort = process.env.LABWEAVER_E2E_VM_SSH_PORT?.trim() ?? ''
  const values = [providerBinding, storageClassBinding, baseDiskBinding, sourceRegistryDigest, capacityBytes, sshPort]
  if (values.every((value) => value === '')) return null
  if (values.some((value) => value === '')) throw new Error('LABWEAVER_E2E_VM_FIELDS_INCOMPLETE')
  if (!VM_BINDING.test(providerBinding)) throw new Error('LABWEAVER_E2E_VM_PROVIDER_BINDING_INVALID')
  if (!VM_BINDING.test(storageClassBinding)) throw new Error('LABWEAVER_E2E_VM_STORAGE_CLASS_BINDING_INVALID')
  if (!VM_BINDING.test(baseDiskBinding)) throw new Error('LABWEAVER_E2E_VM_BASE_DISK_BINDING_INVALID')
  if (!VM_SOURCE_REGISTRY_DIGEST.test(sourceRegistryDigest)) throw new Error('LABWEAVER_E2E_VM_BASE_DISK_DIGEST_INVALID')
  if (!/^[1-9][0-9]{8,15}$/.test(capacityBytes)) throw new Error('LABWEAVER_E2E_VM_BASE_DISK_CAPACITY_INVALID')
  const parsedCapacityBytes = Number(capacityBytes)
  if (!Number.isSafeInteger(parsedCapacityBytes)) throw new Error('LABWEAVER_E2E_VM_BASE_DISK_CAPACITY_INVALID')
  if (sshPort !== '22') throw new Error('LABWEAVER_E2E_VM_SSH_PORT_INVALID')
  return Object.freeze({
    providerBinding,
    storageClassBinding,
    baseDisk: Object.freeze({
      binding: baseDiskBinding,
      sourceRegistryDigest,
      capacityBytes: parsedCapacityBytes,
    }),
    sshPort: Number(sshPort),
  })
}

/**
 * Build transient material for the normal Work authoring path. Container Work
 * carries an explicit generated recipe; VM Work carries the exact reviewed
 * imported-base bindings and is resolved by Control without a build request.
 */
export async function createRealWorkPackage(
  goldenBaseImage,
  { gpu = null, providerBinding = 'kubernetes-work-local-hostpath', vm = null } = {},
) {
  const base = vm ? null : requireDigestPinnedImage(goldenBaseImage)
  if (vm && (
    !VM_BINDING.test(vm.providerBinding ?? '')
    || !VM_BINDING.test(vm.storageClassBinding ?? '')
    || !VM_BINDING.test(vm.baseDisk?.binding ?? '')
    || !VM_SOURCE_REGISTRY_DIGEST.test(vm.baseDisk?.sourceRegistryDigest ?? '')
    || !Number.isSafeInteger(vm.baseDisk?.capacityBytes)
    || vm.baseDisk.capacityBytes <= 0
    || vm.sshPort !== 22
  )) {
    throw new Error('REAL_WORK_VM_CONFIGURATION_INVALID')
  }
  const vmStorageBytes = vm
    ? Math.max(16 * GIB, Math.ceil(vm.baseDisk.capacityBytes / GIB) * GIB)
    : null
  if (vm && !Number.isSafeInteger(vmStorageBytes)) throw new Error('REAL_WORK_VM_STORAGE_CAPACITY_INVALID')
  const directory = await mkdtemp(join(tmpdir(), 'labweaver-real-work-'))
  const seedMarker = vm ? null : `labweaver-real-work-seed-${uuidv7()}`
  const persistenceMarker = `labweaver-real-work-persistence-${uuidv7()}`
  const dockerfile = vm ? null : [
    `FROM ${base.image}`,
    'COPY seed.txt /opt/labweaver/workspace-seed/seed.txt',
    'USER 0',
    'RUN mkdir -p /workspace /tmp /opt/labweaver/workspace-seed && chmod -R a+rX /opt/labweaver/workspace-seed && chmod 0777 /workspace /tmp',
    'USER 65534:65534',
    'WORKDIR /workspace',
    'CMD ["python3", "-m", "http.server", "8080", "--bind", "0.0.0.0", "--directory", "/workspace"]',
    '',
  ].join('\n')
  const environmentSpec = {
    apiVersion: 'environment.labweaver.io/v1',
    kind: 'EnvironmentSpec',
    name: 'labweaver-real-work',
    class: 'work',
    resources: {
      cpuMillicores: 1000,
      memoryBytes: 2 * 1024 * 1024 * 1024,
      storageBytes: vm ? vmStorageBytes : 10 * GIB,
      ...(gpu ? { gpu: { class: gpu.class, count: gpu.count } } : {}),
    },
    network: { mode: 'deny_all' },
    entries: vm
      ? [{ name: 'ssh', protocol: 'ssh', servicePort: vm.sshPort }]
      : [{ name: 'workspace-files', protocol: 'http', servicePort: 8080 }],
    security: {
      userPolicy: 'non_root_required',
      rootFilesystemPolicy: vm ? 'mutable_required' : 'read_only_required',
      privilegeEscalationPolicy: 'deny',
      publicExposurePolicy: 'deny',
      securityProfileBinding: 'restricted-v1',
    },
    runtime: vm
      ? {
        kind: 'virtual_machine',
        provider_binding: vm.providerBinding,
        base_disk: { ...vm.baseDisk },
        storage_class_binding: vm.storageClassBinding,
        ssh_port: vm.sshPort,
      }
      : {
        kind: 'container',
        provider_binding: providerBinding,
        build_recipe: {
          mode: 'generated',
          files: [
            { path: 'Dockerfile', content: dockerfile },
            { path: 'seed.txt', content: `${seedMarker}\n` },
          ],
        },
        service_port: 8080,
        ...(gpu ? { terminal: { executable: '/bin/sh', args: [], workingDirectory: '/workspace' } } : {}),
      },
    retention: {
      policyId: uuidv7(),
      policyRevision: 1,
      class: 'run_evidence',
      retainUntil: new Date(Date.now() + 24 * 60 * 60 * 1000).toISOString(),
      disposition: 'delete',
    },
  }
  const readme = vm
    ? [
      '# Real VM Work package',
      '',
      'Return the nested environmentSpec as a Work environment and preserve its reviewed VM runtime bindings exactly.',
      `Use provider binding ${vm.providerBinding}, storage class binding ${vm.storageClassBinding}, base disk binding ${vm.baseDisk.binding}, source registry digest ${vm.baseDisk.sourceRegistryDigest}, capacity ${vm.baseDisk.capacityBytes} bytes, and SSH port ${vm.sshPort}.`,
      'Keep the SSH entry on port 22, deny-all networking, non-root SSH access, and a mutable guest root filesystem. Do not replace the VM with a container or add a container build recipe.',
      'The guest workspace is writable through its normal authorized SSH access. Do not add SSH keys, passwords, certificates, or other credentials to this package.',
      ...(gpu ? [`The Work resource request must include GPU class ${gpu.class} with count ${gpu.count}.`] : []),
      '',
    ].join('\n')
    : [
      '# Real Work provider package',
      '',
      'This package is used only by the explicitly opted-in real provider E2E path.',
      'Return the nested environmentSpec exactly, adapting only the generated container build recipe.',
      `The Work must remain class=work and use the ${providerBinding} provider binding.`,
      `The generated Dockerfile must start FROM ${base.image}, copy seed.txt into /opt/labweaver/workspace-seed/seed.txt, and run the Python HTTP service on port 8080 as UID/GID 65534.`,
      `The initial workspace must expose the exact seed marker ${seedMarker} through the HTTP endpoint.`,
      ...(gpu ? [
        `The Work resource request must include GPU class ${gpu.class} with count 1.`,
        'Preserve runtime.terminal exactly: executable /bin/sh, args [], workingDirectory /workspace, alongside the HTTP service and seed file.',
        'The owner must run a real CUDA Driver API/PTX kernel through the product terminal: 256 threads, sum 32640, max 255, before and after restart.',
        'Python3 is required. libcuda.so.1 must be supplied by the normal NVIDIA runtime; missing CUDA must fail without CPU fallback or mocks.',
      ] : []),
      '',
    ].join('\n')
  const configurationInstructions = [
    '# Work configuration instructions',
    '',
    vm
      ? 'The approved VM Work configuration runs from the guest workspace. As the authorized non-root guest user, write the exact persistence marker below to persistence-marker.txt in the current directory.'
      : 'For the WorkConfiguration request, write the exact persistence marker below to /workspace/persistence-marker.txt.',
    `Persistence marker: ${persistenceMarker}`,
    'Use a complete executable POSIX shell script and a separate verification script.',
    vm
      ? 'The verification script must fail if persistence-marker.txt in the current guest workspace is missing or has another value. Do not use sudo or alter the SSH access policy.'
      : 'The verification script must fail if /workspace/persistence-marker.txt is missing or has another value.',
    'This file-only change does not require a Work restart.',
    '',
  ].join('\n')

  await writeFile(join(directory, 'README.md'), readme, 'utf8')
  await writeFile(join(directory, 'environment-spec.json'), `${JSON.stringify({ environmentSpec }, null, 2)}\n`, 'utf8')
  await writeFile(join(directory, 'work-configuration.md'), configurationInstructions, 'utf8')

  return {
    directory,
    seedMarker,
    persistenceMarker,
    environmentSpec,
    dockerfile,
    async cleanup() {
      await rm(directory, { recursive: true, force: true })
    },
  }
}

function sameContainerArtifact(actual, expected, code) {
  const actualBuildRequestId = actual?.build_request_id ?? actual?.buildRequestId
  const expectedBuildRequestId = expected?.build_request_id ?? expected?.buildRequestId
  if (
    actual?.kind !== 'container'
    || expected?.kind !== 'container'
    || actual.id !== expected.id
    || actual.repository !== expected.repository
    || actual.digest?.toLowerCase() !== expected.digest?.toLowerCase()
    || actualBuildRequestId !== expectedBuildRequestId
  ) {
    throw new Error(code)
  }
  return actual
}

function sameVirtualMachineArtifact(actual, expected, code) {
  if (
    actual?.kind !== 'virtual_machine'
    || expected?.kind !== 'virtual_machine'
    || actual.id !== expected.id
    || actual.format !== expected.format
    || actual.base_disk?.binding !== expected.base_disk?.binding
    || actual.base_disk?.capacityBytes !== expected.base_disk?.capacityBytes
    || actual.base_disk?.sourceRegistryDigest?.toLowerCase() !== expected.base_disk?.sourceRegistryDigest?.toLowerCase()
  ) {
    throw new Error(code)
  }
  return actual
}

/** Require the approved GPU Work container to support its owner's real CUDA terminal. */
export function assertRealWorkGpuContainerCandidate(candidate, providerBinding, gpu) {
  const spec = candidate?.candidate?.spec
  const runtime = spec?.runtime
  const terminal = runtime?.terminal
  if (spec?.class !== 'work'
    || runtime?.kind !== 'container'
    || runtime.provider_binding !== providerBinding
    || !gpu || gpu.count !== 1
    || spec.resources?.gpu?.class !== gpu.class
    || spec.resources.gpu.count !== 1
    || terminal?.executable !== '/bin/sh'
    || !Array.isArray(terminal.args) || terminal.args.length !== 0
    || terminal.workingDirectory !== '/workspace') {
    throw new Error('REAL_WORK_GPU_CONTAINER_CANDIDATE_SPEC_INVALID')
  }
}

/** Validate the Control-projected artifact for a fresh VM Work candidate. */
export function assertRealWorkVmCandidate(candidate, vm, gpu = null) {
  if (
    !VM_BINDING.test(vm?.providerBinding ?? '')
    || !VM_BINDING.test(vm?.storageClassBinding ?? '')
    || !VM_BINDING.test(vm?.baseDisk?.binding ?? '')
    || !VM_SOURCE_REGISTRY_DIGEST.test(vm?.baseDisk?.sourceRegistryDigest ?? '')
    || !Number.isSafeInteger(vm?.baseDisk?.capacityBytes)
    || vm.baseDisk.capacityBytes <= 0
  ) {
    throw new Error('REAL_WORK_VM_CONFIGURATION_INVALID')
  }
  const spec = candidate?.candidate?.spec
  const runtime = spec?.runtime
  const resources = spec?.resources
  const expectedStorageBytes = Math.max(
    16 * GIB,
    Math.ceil(vm.baseDisk.capacityBytes / GIB) * GIB,
  )
  if (!Number.isSafeInteger(expectedStorageBytes)) throw new Error('REAL_WORK_VM_CONFIGURATION_INVALID')
  if (
    spec?.class !== 'work'
    || runtime?.kind !== 'virtual_machine'
    || runtime.provider_binding !== vm.providerBinding
    || runtime.storage_class_binding !== vm.storageClassBinding
    || runtime.ssh_port !== 22
    || runtime.base_disk?.binding !== vm.baseDisk.binding
    || runtime.base_disk?.capacityBytes !== vm.baseDisk.capacityBytes
    || runtime.base_disk?.sourceRegistryDigest?.toLowerCase() !== vm.baseDisk.sourceRegistryDigest.toLowerCase()
    || Object.hasOwn(runtime, 'build_recipe')
    || candidate.build != null
    || !Array.isArray(spec.entries)
    || spec.entries.length !== 1
    || spec.entries[0].name !== 'ssh'
    || spec.entries[0].protocol !== 'ssh'
    || spec.entries[0].servicePort !== 22
    || spec.security?.userPolicy !== 'non_root_required'
    || spec.security?.rootFilesystemPolicy !== 'mutable_required'
    || spec.security?.privilegeEscalationPolicy !== 'deny'
    || spec.security?.publicExposurePolicy !== 'deny'
    || spec.network?.mode !== 'deny_all'
    || resources?.cpuMillicores !== 1000
    || resources?.memoryBytes !== 2 * GIB
    || resources?.storageBytes !== expectedStorageBytes
    || (gpu && (resources?.gpu?.class !== gpu.class || resources?.gpu?.count !== gpu.count))
  ) {
    throw new Error('REAL_WORK_VM_CANDIDATE_SPEC_INVALID')
  }

  const artifact = candidate.imageArtifact
  if (
    !artifact
    || artifact.kind !== 'virtual_machine'
    || typeof artifact.id !== 'string'
    || artifact.id.trim() === ''
    || !VM_DISK_FORMATS.includes(artifact.format)
    || artifact.base_disk?.binding !== vm.baseDisk.binding
    || artifact.base_disk?.capacityBytes !== vm.baseDisk.capacityBytes
    || artifact.base_disk?.sourceRegistryDigest?.toLowerCase() !== vm.baseDisk.sourceRegistryDigest.toLowerCase()
  ) {
    throw new Error('REAL_WORK_VM_CANDIDATE_ARTIFACT_INVALID')
  }
  return artifact
}

function resumableContainerArtifact(candidate) {
  const artifact = candidate?.imageArtifact ?? candidate?.build?.artifact
  if (!artifact || artifact.kind !== 'container') throw new Error('REAL_WORK_RESUME_CONTAINER_ARTIFACT_MISSING')
  if (typeof artifact.repository !== 'string' || artifact.repository.trim() === '') {
    throw new Error('REAL_WORK_RESUME_CONTAINER_REPOSITORY_MISSING')
  }
  if (!/^sha256:[0-9a-f]{64}$/i.test(artifact.digest ?? '')) {
    throw new Error('REAL_WORK_RESUME_CONTAINER_DIGEST_INVALID')
  }
  return artifact
}

async function readResumeActorId(request) {
  const session = await expectJson(await request.get('/api/v1/auth/session'), 'REAL_WORK_RESUME_SESSION_READ_FAILED')
  const actorId = session.actor?.actorId
  if (typeof actorId !== 'string' || actorId.length === 0) throw new Error('REAL_WORK_RESUME_ACTOR_ID_MISSING')
  return actorId
}

/**
 * Validate a previously completed Work authoring/build/publication chain by
 * its exact IDs. This gate intentionally stops before any resume-only marker
 * checks so other recovery journeys can reuse the same immutable chain proof.
 */
export async function readPublishedWork(request, resume, { gpu = null, vm = null } = {}) {
  const { projectId, runId, releaseId } = resume
  const actorId = await readResumeActorId(request)
  const project = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}`),
    'REAL_WORK_RESUME_PROJECT_READ_FAILED',
  )
  if (project.id !== projectId || project.ownerActorId !== actorId) {
    throw new Error('REAL_WORK_RESUME_PROJECT_OWNERSHIP_INVALID')
  }

  const run = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/agent-runs/${encodeURIComponent(runId)}`),
    'REAL_WORK_RESUME_AGENT_RUN_READ_FAILED',
  )
  const environmentTrack = run.tracks?.find((track) => track.kind === 'environment')
  if (
    run.id !== runId
    || run.projectId !== projectId
    || run.state !== 'succeeded'
    || run.purpose?.kind !== 'authoring'
    || run.purpose.environmentClass !== 'work'
    || !Array.isArray(run.tracks)
    || run.tracks.length !== 1
    || !environmentTrack?.candidateId
    || typeof run.packageId !== 'string'
  ) {
    throw new Error('REAL_WORK_RESUME_AGENT_RUN_INVALID')
  }

  const candidateView = await expectJson(
    await request.get(
      `/api/v1/projects/${encodeURIComponent(projectId)}/environment-candidates/${encodeURIComponent(environmentTrack.candidateId)}`,
    ),
    'REAL_WORK_RESUME_ENVIRONMENT_CANDIDATE_READ_FAILED',
  )
  const candidate = candidateView.candidate
  if (
    candidate?.id !== environmentTrack.candidateId
    || candidate.runId !== runId
    || candidate.projectId !== projectId
    || candidate.spec?.class !== 'work'
    || candidateView.imageArtifact == null
  ) {
    throw new Error('REAL_WORK_RESUME_CANDIDATE_INVALID')
  }
  const runtime = candidate.spec.runtime
  const candidateBuildReady = runtime?.kind === 'virtual_machine'
    ? candidateView.build == null || candidateView.build.state === 'succeeded'
    : candidateView.build?.state === 'succeeded'
  if (!candidateBuildReady) throw new Error('REAL_WORK_RESUME_CANDIDATE_BUILD_INVALID')
  if (gpu) {
    const declaredGpu = candidate.spec?.resources?.gpu
    if (declaredGpu?.class !== gpu.class || declaredGpu?.count !== gpu.count) {
      throw new Error('REAL_WORK_RESUME_GPU_SPEC_MISMATCH')
    }
  }
  if ((runtime?.kind === 'virtual_machine') !== Boolean(vm)) {
    throw new Error('REAL_WORK_RESUME_RUNTIME_CONFIGURATION_MISMATCH')
  }
  const runtimeArtifact = vm
    ? candidateView.imageArtifact
    : resumableContainerArtifact(candidateView)
  if (vm) {
    if (
      runtime?.provider_binding !== vm.providerBinding
      || runtime.storage_class_binding !== vm.storageClassBinding
      || runtime.ssh_port !== vm.sshPort
      || runtime.base_disk?.binding !== vm.baseDisk.binding
      || runtime.base_disk?.capacityBytes !== vm.baseDisk.capacityBytes
      || runtime.base_disk?.sourceRegistryDigest?.toLowerCase() !== vm.baseDisk.sourceRegistryDigest.toLowerCase()
      || runtimeArtifact?.kind !== 'virtual_machine'
      || runtimeArtifact.base_disk?.binding !== vm.baseDisk.binding
      || runtimeArtifact.base_disk?.capacityBytes !== vm.baseDisk.capacityBytes
      || runtimeArtifact.base_disk?.sourceRegistryDigest?.toLowerCase() !== vm.baseDisk.sourceRegistryDigest.toLowerCase()
    ) {
      throw new Error('REAL_WORK_RESUME_VM_BINDING_INVALID')
    }
    if (!VM_DISK_FORMATS.includes(runtimeArtifact.format)) throw new Error('REAL_WORK_RESUME_VM_ARTIFACT_FORMAT_INVALID')
  } else {
    sameContainerArtifact(runtimeArtifact, candidateView.build.artifact, 'REAL_WORK_RESUME_BUILD_ARTIFACT_INVALID')
    sameContainerArtifact(runtimeArtifact, candidateView.imageArtifact, 'REAL_WORK_RESUME_CANDIDATE_ARTIFACT_INVALID')
  }
  const candidateApproval = Array.isArray(candidateView.approvals)
    ? candidateView.approvals.find((item) => (
      item.candidateId === candidate.id
      && item.candidateRevision === candidate.revision
      && item.policyRevision === candidate.policyRevision
      && item.trustRevision === candidateView.trustRevision
      && item.decision === 'approved'
    ))
    : undefined
  if (!candidateApproval) throw new Error('REAL_WORK_RESUME_CANDIDATE_NOT_APPROVED')

  const release = await expectJson(
    await request.get(
      `/api/v1/projects/${encodeURIComponent(projectId)}/environment-template-releases/${encodeURIComponent(releaseId)}`,
    ),
    'REAL_WORK_RESUME_RELEASE_READ_FAILED',
  )
  if (
    release.id !== releaseId
    || release.projectId !== projectId
    || release.agentRunId !== runId
    || release.candidateId !== candidate.id
    || release.candidateRevision !== candidate.revision
    || !Number.isInteger(release.version)
    || release.version < 1
    || release.runtimeKind !== (vm ? 'virtual_machine' : 'container')
    || release.approval?.id !== candidateApproval.id
    || release.approval?.candidateId !== candidate.id
    || release.approval?.candidateRevision !== candidate.revision
    || release.approval?.policyRevision !== candidateApproval.policyRevision
    || release.approval?.trustRevision !== candidateApproval.trustRevision
    || release.approval?.decision !== 'approved'
  ) {
    throw new Error('REAL_WORK_RESUME_RELEASE_BINDING_INVALID')
  }
  if (vm) sameVirtualMachineArtifact(release.artifact, runtimeArtifact, 'REAL_WORK_RESUME_RELEASE_ARTIFACT_INVALID')
  else sameContainerArtifact(release.artifact, runtimeArtifact, 'REAL_WORK_RESUME_RELEASE_ARTIFACT_INVALID')

  // The release binds this exact candidate revision; retention lives on its spec.
  const retainUntil = candidate.spec.retention?.retainUntil
  const retentionDeadline = typeof retainUntil === 'string' ? Date.parse(retainUntil) : NaN
  const retentionRecovery = 'Generate, review, and publish a new Work template through the normal UI before retrying.'
  if (!Number.isFinite(retentionDeadline)) {
    throw new Error(`REAL_WORK_RESUME_RETENTION_INVALID: ${retentionRecovery}`)
  }
  if (retentionDeadline <= Date.now()) {
    throw new Error(`REAL_WORK_RESUME_RETENTION_EXPIRED: ${retentionRecovery}`)
  }

  const packageData = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/problem-packages/${encodeURIComponent(run.packageId)}`),
    'REAL_WORK_RESUME_PACKAGE_READ_FAILED',
  )
  if (packageData.id !== run.packageId || packageData.projectId !== projectId || !Number.isInteger(packageData.revision) || packageData.revision < 1) {
    throw new Error('REAL_WORK_RESUME_PACKAGE_INVALID')
  }

  return {
    project,
    run,
    candidateView,
    release,
    packageData,
  }
}

/**
 * Resume a previously completed Work chain after validating the immutable
 * published chain and the resume-only persistence markers.
 */
export async function readResumablePublishedWork(request, resume, { gpu = null, vm = null } = {}) {
  const published = await readPublishedWork(request, resume, { gpu, vm })
  if ((!vm && !resume.seedMarker) || !resume.persistenceMarker) {
    throw new Error(vm ? 'REAL_WORK_RESUME_VM_PERSISTENCE_MARKER_REQUIRED' : 'REAL_WORK_RESUME_MARKERS_REQUIRED')
  }
  return {
    ...published,
    seedMarker: resume.seedMarker,
    persistenceMarker: resume.persistenceMarker,
  }
}

function currentRateDimension(rate, target, now = Date.now()) {
  const effectiveFrom = Date.parse(rate?.effectiveFrom)
  const effectiveUntil = rate?.effectiveUntil == null ? null : Date.parse(rate.effectiveUntil)
  if (
    !rate
    || rate.unit !== target.unit
    || !Number.isSafeInteger(rate.unitQuantity)
    || rate.unitQuantity < 1
    || !Number.isFinite(effectiveFrom)
    || effectiveFrom > now
    || (effectiveUntil !== null && (!Number.isFinite(effectiveUntil) || effectiveUntil <= now))
  ) return false
  if (target.unit === 'gpu_unit_second') {
    return rate.gpuClass === target.gpuClass && rate.gpuMode === target.gpuMode
  }
  return rate.gpuClass == null && rate.gpuMode == null
}

function rateMatchesInput(rate, target) {
  const boundaryMatches = target.effectiveUntil == null || (() => {
    const expected = Date.parse(target.effectiveUntil)
    const actual = Date.parse(rate?.effectiveUntil ?? '')
    return Number.isFinite(expected) && Number.isFinite(actual)
      && Math.floor(expected / 60_000) === Math.floor(actual / 60_000)
  })()
  return boundaryMatches
    && currentRateDimension(rate, target)
    && rate.unitQuantity === target.unitQuantity
    && rate.unitPrice?.currency === target.currency
    && rate.unitPrice?.amount === target.amount
}

function hasPositiveRatePrice(rate) {
  return typeof rate?.unitPrice?.currency === 'string'
    && rate.unitPrice.currency.length > 0
    && FIXED_DECIMAL.test(rate.unitPrice.amount ?? '')
    && Number(rate.unitPrice.amount) > 0
}

async function readResourceRatesInternal(context, diagnosticCode) {
  const rates = await expectJson(
    await context.request.get('/api/v1/resource/rates'),
    diagnosticCode,
  )
  if (!Array.isArray(rates)) throw new Error('REAL_WORK_RATES_RESPONSE_INVALID')
  if (rates.some((rate) => !Number.isSafeInteger(rate?.unitQuantity) || rate.unitQuantity < 1)) {
    throw new Error('REAL_WORK_RATE_QUANTITY_INVALID')
  }
  return rates
}

function localDateTimeValue(date = new Date()) {
  const pad = (value) => String(value).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

async function waitForRateListUi(page, unit) {
  const form = page.getByTestId('resource-rate-form')
  await expect(form).toBeVisible({ timeout: 120_000 })
  await expect.poll(
    async () => {
      const errorBanner = page.locator('section.rates-card [role="alert"]')
      if (await errorBanner.count()) {
        const code = (await errorBanner.first().locator('.diagnostic-code').textContent())?.trim() || 'diagnostic-missing'
        throw new Error(`REAL_WORK_${unit.toUpperCase()}_RATES_LOAD_FAILED:${code}`)
      }
      return (await page.locator('section.rates-card ul[aria-label="资源费率列表"]').count()) > 0
        || (await page.getByText('还没有资源费率。创建费率后，匹配的 GPU 目录项才能用于资源申请。', { exact: true }).count()) > 0
    },
    { timeout: 120_000, intervals: [250, 500, 1000] },
  ).toBe(true)
}

async function readRateRowsFromUi(page) {
  const list = page.locator('section.rates-card ul[aria-label="资源费率列表"]')
  if (await list.count() === 0) return []
  return list.locator('li.rate-row').evaluateAll((rows) => rows.map((row) => ({
    label: row.querySelector('.rate-main strong')?.textContent?.trim() || '',
    detail: row.querySelector('.rate-main small')?.textContent?.trim() || '',
    revision: Number(row.querySelector('.state-chip')?.textContent?.match(/版本\s+(\d+)/)?.[1] || NaN),
  })))
}

async function createRateByUi(page, target) {
  const form = page.getByTestId('resource-rate-form')
  await form.getByLabel('计费单位', { exact: true }).selectOption(target.unit)
  if (target.unit === 'gpu_unit_second') {
    const gpuSelect = form.getByLabel('GPU 目录分配类型', { exact: true })
    const gpuOption = `${target.gpuClass}:${target.gpuMode}`
    await expect(gpuSelect.locator(`option[value="${gpuOption}"]`)).toHaveText(
      `${target.gpuClass} · ${({ exclusive: '独占', container_time_slice: '容器时间片', vm_vgpu: 'VM vGPU' })[target.gpuMode]}`,
    )
    await gpuSelect.selectOption(gpuOption)
  }
  await form.getByLabel('费率单价', { exact: true }).fill(target.amount)
  await form.getByLabel('币种', { exact: true }).fill(target.currency)
  const effectiveFromMs = Math.ceil((Date.now() + 10_000) / 60_000) * 60_000
  const effectiveUntilMs = target.effectiveUntil == null ? null : Date.parse(target.effectiveUntil)
  if (effectiveUntilMs !== null && (!Number.isFinite(effectiveUntilMs) || effectiveUntilMs <= effectiveFromMs)) {
    throw new Error(`REAL_WORK_RATE_WINDOW_INVALID:${target.unit}`)
  }
  await form.getByLabel('生效时间', { exact: true }).fill(localDateTimeValue(new Date(effectiveFromMs)))
  await form.getByLabel('结束时间（可选）', { exact: true }).fill(
    effectiveUntilMs === null ? '' : localDateTimeValue(new Date(effectiveUntilMs)),
  )
  const baseUnit = ({
    gpu_unit_second: 'GPU 分配单位秒',
    cpu_millicore_second: 'CPU millicore 秒',
    memory_byte_second: '内存字节秒',
    storage_byte_second: '存储字节秒',
  })[target.unit]
  await expect(form.getByRole('status')).toContainText(`实际提交：${target.unitQuantity} ${baseUnit}`)

  const createButton = form.getByRole('button', { name: '创建费率版本', exact: true })
  await expect(createButton).toBeEnabled()
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST' && url.pathname === '/api/v1/resource/rates'
  })
  await createButton.click()
  const response = await responsePromise
  if (!response.ok()) throw new Error(`REAL_WORK_${target.unit.toUpperCase()}_RATE_CREATE_FAILED:status-${response.status()}`)
  await expect(page.locator('.diagnostic-banner').filter({ hasText: '资源费率已创建。' })).toBeVisible({ timeout: 120_000 })
}

async function waitForRateUiReadback(page, target, revision) {
  const expectedLabel = target.unit === 'gpu_unit_second'
    ? `GPU ${target.gpuClass} · ${({ exclusive: '独占', container_time_slice: '容器时间片', vm_vgpu: 'VM vGPU' })[target.gpuMode]}`
    : ({ cpu_millicore_second: 'CPU', memory_byte_second: '内存', storage_byte_second: '存储' })[target.unit]
  await expect.poll(async () => {
    const rows = await readRateRowsFromUi(page)
    return rows.some((row) => (
      row.label === expectedLabel
      && row.detail.startsWith(`${target.unitQuantity} 基础单位 · ${target.amount} ${target.currency} ·`)
      && row.revision === revision
    ))
  }, { timeout: 120_000, intervals: [250, 500, 1000] }).toBe(true)
}

export async function waitForActiveRateReadback(context, target) {
  let current = []
  await expect.poll(
    async () => {
      const rates = await readResourceRates(context, 'REAL_WORK_RATES_READBACK_FAILED')
      current = rates.filter((rate) => currentRateDimension(rate, target))
      return current.length
    },
    { timeout: 120_000, intervals: [250, 500, 1000] },
  ).toBe(1)
  return current[0]
}

export async function ensureRateByUi(page, context, target) {
  let rates = await readResourceRates(context, 'REAL_WORK_RATES_LIST_FAILED')
  let current = rates.filter((rate) => currentRateDimension(rate, target))
  if (current.length > 1) throw new Error(`REAL_WORK_RATE_ACTIVE_AMBIGUOUS:${target.unit}`)
  if (current.length === 1) {
    if (!hasPositiveRatePrice(current[0])) throw new Error(`REAL_WORK_RATE_ACTIVE_INVALID:${target.unit}`)
    if (target.unit === 'gpu_unit_second' && !rateMatchesInput(current[0], target)) {
      throw new Error(`REAL_WORK_GPU_RATE_ACTIVE_CONFLICT:${target.gpuClass}`)
    }
    return current[0]
  }

  if (rates.some((rate) => rate.unit === target.unit
    && (target.unit === 'gpu_unit_second'
      ? rate.gpuClass === target.gpuClass && rate.gpuMode === target.gpuMode
      : rate.gpuClass == null && rate.gpuMode == null)
    && Date.parse(rate.effectiveFrom) > Date.now())) {
    throw new Error(`REAL_WORK_RATE_FUTURE_CONFIGURED:${target.unit}`)
  }

  await createRateByUi(page, target)
  current = [await waitForActiveRateReadback(context, target)]
  if (!rateMatchesInput(current[0], target)) {
    throw new Error(`REAL_WORK_RATE_ACTIVE_READBACK_MISMATCH:${target.unit}`)
  }
  await waitForRateUiReadback(page, target, current[0].revision)
  return current[0]
}

/**
 * Ensure the Work dimensions have effective rates through the administrator's
 * finance form. Existing active CPU, memory, and storage rates are reused at
 * their operator-selected values. An explicitly requested GPU dimension must
 * match its requested amount and currency, otherwise the run fails closed
 * instead of replacing a global rate used by other projects.
 */
export async function ensureRealWorkRates(browser, baseURL, { gpu = null } = {}) {
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  const page = await context.newPage()
  try {
    await navigateFromHomeByUi(page, '预算与费用')
    await expect(page.getByRole('heading', { name: '费率、预算与费用', exact: true })).toBeVisible({ timeout: 120_000 })
    await waitForRateListUi(page, 'resource')
    const targets = [...DEFAULT_RATE_INPUTS, ...(gpu ? [gpu.rate] : [])]
    const rates = []
    for (const target of targets) rates.push(await ensureRateByUi(page, context, target))
    return rates
  } finally {
    await context.close()
  }
}

export async function configureRealWorkBudgetByUi(browser, baseURL, projectId) {
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  const page = await context.newPage()
  try {
    await navigateFromHomeByUi(page, '预算与费用')
    await expect(page.getByRole('heading', { name: '费率、预算与费用', exact: true })).toBeVisible({ timeout: 120_000 })
    const projectSelect = page.locator('.project-strip select')
    await expect(projectSelect.locator(`option[value="${projectId}"]`)).toHaveCount(1, { timeout: 120_000 })
    await projectSelect.selectOption(projectId)
    await expect(page.locator('.budget-form')).toBeVisible({ timeout: 120_000 })
    const inputs = page.locator('.budget-form input')
    await expect(inputs).toHaveCount(3)
    const currency = inputs.nth(0)
    if (!(await currency.isEditable())) {
      await expect(currency).toHaveValue('USD')
    } else {
      await currency.fill('USD')
    }
    await inputs.nth(1).fill('1000000000.000000')
    await inputs.nth(2).fill('900000000.000000')
    const responsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'PUT'
        && url.pathname === `/api/v1/projects/${projectId}/resource-budget`
    })
    await page.locator('.budget-form button[type="submit"]').click()
    await expectJson(await responsePromise, 'REAL_WORK_RESOURCE_BUDGET_SAVE_FAILED')
    await expect(page.locator('.budget-summary')).toBeVisible({ timeout: 120_000 })
  } finally {
    await context.close()
  }
}

export function assertRealWorkCharges(charges, gpu = null) {
  if (!Array.isArray(charges)) throw new Error('REAL_WORK_CHARGES_RESPONSE_INVALID')
  const positiveSettled = charges.filter((charge) => charge.settlement === 'settled' && Number(charge.total?.amount) > 0)
  const units = new Set(positiveSettled.flatMap((charge) => (charge.lines ?? []).filter((line) => Number(line.amount?.amount) > 0).map((line) => line.unit)))
  for (const unit of BILLING_UNITS) {
    if (!units.has(unit)) throw new Error(`REAL_WORK_POSITIVE_SETTLED_CHARGE_MISSING:${unit}`)
  }
  if (gpu && !units.has('gpu_unit_second')) throw new Error('REAL_WORK_POSITIVE_SETTLED_CHARGE_MISSING:gpu_unit_second')
  return positiveSettled
}

export async function waitForRealWorkCharges(browser, baseURL, projectId, { gpu = null } = {}) {
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  try {
    let latest
    await expect.poll(async () => {
      const response = await context.request.get(`/api/v1/projects/${projectId}/charges`)
      latest = await expectJson(response, 'REAL_WORK_CHARGES_READ_FAILED')
      try {
        assertRealWorkCharges(latest, gpu)
        return true
      } catch {
        return false
      }
    }, { timeout: 300_000, intervals: [1000, 2000, 3000] }).toBe(true)
    assertRealWorkCharges(latest, gpu)
    const budget = await expectJson(
      await context.request.get(`/api/v1/projects/${projectId}/resource-budget`),
      'REAL_WORK_BUDGET_READ_FAILED',
    )
    if (budget.limit?.currency !== 'USD' || Number(budget.spent?.amount) <= 0) {
      throw new Error('REAL_WORK_BUDGET_SPENT_NOT_POSITIVE')
    }
    return { charges: latest, budget }
  } finally {
    await context.close()
  }
}

export async function inspectRealWorkFinanceByUi(browser, baseURL, projectId, {
  gpu = null,
  usageRecordIds = [],
  expectedCharges = [],
  requireBudget = true,
} = {}) {
  if (!Array.isArray(usageRecordIds) || usageRecordIds.some((id) => typeof id !== 'string' || id === '')) {
    throw new Error('REAL_WORK_FINANCE_USAGE_IDS_INVALID')
  }
  if (!Array.isArray(expectedCharges)) throw new Error('REAL_WORK_FINANCE_CHARGES_INVALID')
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  const page = await context.newPage()
  try {
    await navigateFromHomeByUi(page, '预算与费用')
    await expect(page.getByRole('heading', { name: '费率、预算与费用', exact: true })).toBeVisible({ timeout: 120_000 })
    const projectSelect = page.locator('.project-strip select')
    await expect(projectSelect.locator(`option[value="${projectId}"]`)).toHaveCount(1, { timeout: 120_000 })
    await projectSelect.selectOption(projectId)
    await expect(page.locator('.charge-row').first()).toBeVisible({ timeout: 120_000 })
    const spent = page
      .locator('.budget-summary > div')
      .filter({ hasText: '已花费' })
      .locator('strong')
    if (requireBudget) {
      await expect(spent).toHaveCount(1, { timeout: 120_000 })
    } else {
      await expect.poll(
        async () => (await spent.count()) === 1
          || (await page.getByText('该项目还没有预算记录。', { exact: true }).count()) === 1,
        { timeout: 120_000, intervals: [500, 1000, 2000] },
      ).toBe(true)
    }
    if (await spent.count() === 1) {
      await expect.poll(
        async () => {
          const text = (await spent.textContent())?.trim() ?? ''
          const [amount, currency] = text.split(/\s+/)
          return currency === 'USD'
            && /^\d+\.\d{6}$/.test(amount ?? '')
            && /[1-9]/.test((amount ?? '').replace('.', ''))
        },
        { timeout: 120_000, intervals: [500, 1000, 2000] },
      ).toBe(true)
    }
    if (gpu) {
      const gpuLine = page.locator('.charge-line').filter({ hasText: 'GPU' })
      await expect(gpuLine.first()).toBeVisible({ timeout: 120_000 })
    }
    const expectedByUsageId = new Map(expectedCharges.map((charge) => [charge.usageRecordId, charge]))
    for (const usageRecordId of usageRecordIds) {
      const row = page.locator('.charge-row').filter({
        has: page.locator('details.advanced-details').filter({ hasText: `用量记录 ID：${usageRecordId}` }),
      })
      await expect(row).toHaveCount(1, { timeout: 120_000 })
      const expected = expectedByUsageId.get(usageRecordId)
      if (!expected) throw new Error(`REAL_WORK_FINANCE_CHARGE_EXPECTATION_MISSING:${usageRecordId}`)
      await expect(row.locator('.state-chip')).toHaveText('已结算')
      await expect(row.locator('.charge-main > strong')).toContainText(
        `${expected.total.amount} ${expected.total.currency}`,
      )
    }
  } finally {
    await context.close()
  }
}

function formatFixedDecimalScaled(value) {
  const negative = value < 0n
  const magnitude = negative ? -value : value
  const whole = magnitude / FIXED_DECIMAL_SCALE
  const fraction = (magnitude % FIXED_DECIMAL_SCALE).toString().padStart(6, '0')
  return `${negative ? '-' : ''}${whole}.${fraction}`
}

function normalizeRealWorkCharge(charge) {
  return {
    id: charge?.id ?? null,
    usageRecordId: charge?.usageRecordId ?? null,
    projectId: charge?.projectId ?? null,
    courseId: charge?.courseId ?? null,
    lines: charge?.lines ?? null,
    total: charge?.total ?? null,
    settlement: charge?.settlement ?? null,
    createdAt: charge?.createdAt ?? null,
    adjustmentOf: charge?.adjustmentOf ?? null,
    adjustmentReason: charge?.adjustmentReason ?? null,
    adjustedBy: charge?.adjustedBy ?? null,
    diagnosticCode: charge?.diagnosticCode ?? null,
  }
}

function sameRealWorkCharge(left, right) {
  return JSON.stringify(normalizeRealWorkCharge(left)) === JSON.stringify(normalizeRealWorkCharge(right))
}

function validateRealWorkAdjustmentCharge(charge, projectId) {
  if (
    !charge
    || typeof projectId !== 'string'
    || projectId === ''
    || typeof charge.id !== 'string'
    || charge.id === ''
    || charge.projectId !== projectId
    || charge.settlement !== 'settled'
    || charge.adjustmentOf != null
    || typeof charge.total?.currency !== 'string'
    || !CURRENCY.test(charge.total.currency)
    || !FIXED_DECIMAL.test(charge.total.amount ?? '')
  ) {
    throw new Error('REAL_WORK_FINANCE_ADJUSTMENT_CHARGE_NOT_ORIGINAL')
  }
  const totalScaled = fixedDecimalScaled(charge.total.amount, 'REAL_WORK_FINANCE_ADJUSTMENT_CHARGE')
  if (totalScaled < 1n) throw new Error('REAL_WORK_FINANCE_ADJUSTMENT_CHARGE_TOO_SMALL')
  return totalScaled
}

async function readRealWorkFinanceSnapshot(request, projectId) {
  const encodedProjectId = encodeURIComponent(projectId)
  const [budgetResponse, chargesResponse] = await Promise.all([
    request.get(`/api/v1/projects/${encodedProjectId}/resource-budget`),
    request.get(`/api/v1/projects/${encodedProjectId}/charges`),
  ])
  const [budget, charges] = await Promise.all([
    expectJson(budgetResponse, 'REAL_WORK_FINANCE_BUDGET_READ_FAILED'),
    expectJson(chargesResponse, 'REAL_WORK_FINANCE_CHARGES_READ_FAILED'),
  ])
  if (!budget || budget.projectId !== projectId || !Array.isArray(charges)) {
    throw new Error('REAL_WORK_FINANCE_SNAPSHOT_INVALID')
  }
  return { budget, charges }
}

function validateRealWorkBudget(budget, projectId) {
  if (
    !budget
    || budget.projectId !== projectId
    || typeof budget.limit?.currency !== 'string'
    || budget.limit.currency !== budget.warningAt?.currency
    || budget.limit.currency !== budget.spent?.currency
    || !CURRENCY.test(budget.limit.currency)
    || !FIXED_DECIMAL.test(budget.limit.amount ?? '')
    || !FIXED_DECIMAL.test(budget.warningAt.amount ?? '')
    || !FIXED_DECIMAL.test(budget.spent.amount ?? '')
  ) {
    throw new Error('REAL_WORK_FINANCE_BUDGET_INVALID')
  }
  const limitScaled = fixedDecimalScaled(budget.limit.amount, 'REAL_WORK_FINANCE_BUDGET')
  const warningScaled = fixedDecimalScaled(budget.warningAt.amount, 'REAL_WORK_FINANCE_BUDGET')
  const spentScaled = fixedDecimalScaled(budget.spent.amount, 'REAL_WORK_FINANCE_BUDGET')
  if (warningScaled > limitScaled) throw new Error('REAL_WORK_FINANCE_BUDGET_WARNING_ABOVE_LIMIT')
  if (spentScaled > limitScaled) throw new Error('REAL_WORK_FINANCE_BUDGET_LIMIT_BELOW_SPENT')
  return { limitScaled, warningScaled, spentScaled }
}

function findRealWorkCharge(charges, chargeId) {
  const matches = charges.filter((charge) => charge?.id === chargeId)
  if (matches.length !== 1) throw new Error(`REAL_WORK_FINANCE_CHARGE_NOT_UNIQUE:${chargeId}`)
  return matches[0]
}

function waitForResponseSafely(page, predicate, options) {
  const responsePromise = page.waitForResponse(predicate, options)
  void responsePromise.catch(() => undefined)
  return responsePromise
}

async function saveRealWorkBudgetByUi(page, projectId, limit, warningAt) {
  const form = page.locator('.budget-form')
  await expect(form).toBeVisible({ timeout: 120_000 })
  const inputs = form.locator('input')
  await expect(inputs).toHaveCount(3)
  await expect(inputs.nth(0)).toHaveValue('USD')
  await inputs.nth(1).fill(limit)
  await inputs.nth(2).fill(warningAt)
  const responsePromise = waitForResponseSafely(page, (response) => {
    const url = new URL(response.url())
    return response.request().method() === 'PUT'
      && url.pathname === `/api/v1/projects/${projectId}/resource-budget`
  })
  await form.locator('button[type="submit"]').click()
  try {
    await expectJson(await responsePromise, 'REAL_WORK_FINANCE_BUDGET_SAVE_FAILED')
  } catch (error) {
    const observed = await readRealWorkFinanceSnapshot(page.request, projectId)
    if (observed.budget.limit.amount !== limit || observed.budget.warningAt.amount !== warningAt) throw error
    return observed
  }
  const observed = await readRealWorkFinanceSnapshot(page.request, projectId)
  if (observed.budget.limit.amount !== limit || observed.budget.warningAt.amount !== warningAt) {
    throw new Error('REAL_WORK_FINANCE_BUDGET_READBACK_MISMATCH')
  }
  return observed
}

function realWorkChargeRow(page, chargeId) {
  return page.locator('.charge-row').filter({
    has: page.locator('details.advanced-details').filter({ hasText: `费用记录 ID：${chargeId}` }),
  })
}

async function waitForRealWorkAdjustment(request, projectId, original, expectedSpent, reason) {
  let latest = null
  let adjustment = null
  await expect.poll(
    async () => {
      latest = await readRealWorkFinanceSnapshot(request, projectId)
      const currentOriginal = findRealWorkCharge(latest.charges, original.id)
      if (!sameRealWorkCharge(currentOriginal, original)) return false
      const candidates = latest.charges.filter((charge) => charge?.adjustmentOf === original.id)
      if (candidates.length !== 1) return false
      adjustment = candidates[0]
      return (
        adjustment.settlement === 'settled'
        && adjustment.total?.currency === original.total.currency
        && adjustment.total?.amount === '-0.000001'
        && adjustment.adjustmentReason === reason
        && latest.budget.spent?.amount === expectedSpent
      )
    },
    { timeout: 120_000, intervals: [500, 1000, 2000] },
  ).toBe(true)
  return { snapshot: latest, adjustment }
}

/**
 * Verify one already-settled Work charge through the administrator UI. The
 * helper changes only the selected project's budget reminder and appends one
 * six-decimal adjustment; all diagnostic reads use GET and the original
 * budget threshold is restored before returning.
 */
export async function verifyRealWorkFinanceAdjustmentByUi(browser, baseURL, projectId, settledCharge) {
  const originalTotalScaled = validateRealWorkAdjustmentCharge(settledCharge, projectId)
  const adjustmentAmount = '-0.000001'
  const adjustmentReason = '试用账目调整'
  if (!browser || typeof browser.newContext !== 'function') throw new Error('REAL_WORK_FINANCE_BROWSER_INVALID')

  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  const page = await context.newPage()
  let budgetOverrideApplied = false
  let budgetRestored = false
  let initialBudget = null
  let failure = null
  let result = null
  try {
    await navigateFromHomeByUi(page, '预算与费用')
    await expect(page.getByRole('heading', { name: '费率、预算与费用', exact: true })).toBeVisible({ timeout: 120_000 })
    const projectSelect = page.locator('.project-strip select')
    await expect(projectSelect.locator(`option[value="${projectId}"]`)).toHaveCount(1, { timeout: 120_000 })
    await projectSelect.selectOption(projectId)

    const initial = await readRealWorkFinanceSnapshot(page.request, projectId)
    const initialBudgetNumbers = validateRealWorkBudget(initial.budget, projectId)
    initialBudget = initial.budget
    if (initial.budget.limit.currency !== 'USD' || settledCharge.total.currency !== 'USD') {
      throw new Error('REAL_WORK_FINANCE_USD_REQUIRED')
    }
    const original = findRealWorkCharge(initial.charges, settledCharge.id)
    if (!sameRealWorkCharge(original, settledCharge)) throw new Error('REAL_WORK_FINANCE_CHARGE_READBACK_MISMATCH')
    if (initial.charges.some((charge) => charge?.adjustmentOf === original.id)) {
      throw new Error('REAL_WORK_FINANCE_CHARGE_ALREADY_ADJUSTED')
    }
    if (originalTotalScaled !== fixedDecimalScaled(original.total.amount, 'REAL_WORK_FINANCE_CHARGE')) {
      throw new Error('REAL_WORK_FINANCE_CHARGE_AMOUNT_CHANGED')
    }
    if (initialBudgetNumbers.spentScaled < 1n) throw new Error('REAL_WORK_FINANCE_BUDGET_SPENT_TOO_SMALL')
    const expectedSpentScaled = initialBudgetNumbers.spentScaled - 1n
    const expectedSpent = formatFixedDecimalScaled(expectedSpentScaled)
    const projectRow = realWorkChargeRow(page, original.id)
    await expect(projectRow).toHaveCount(1, { timeout: 120_000 })
    await expect(projectRow.locator('.state-chip')).toHaveText('已结算')

    budgetOverrideApplied = true
    await saveRealWorkBudgetByUi(page, projectId, initial.budget.limit.amount, initial.budget.spent.amount)
    await expect(page.getByTestId('budget-threshold-warning')).toBeVisible({ timeout: 120_000 })

    const adjustmentRow = realWorkChargeRow(page, original.id)
    await expect(adjustmentRow).toHaveCount(1, { timeout: 120_000 })
    await adjustmentRow.getByRole('button', { name: '调整', exact: true }).click()
    const form = page.locator('.adjustment-form')
    await expect(form).toBeVisible({ timeout: 120_000 })
    await expect(form.locator('details.advanced-details')).toContainText(`费用记录 ID：${original.id}`)
    await form.getByLabel('调整金额（可为负）', { exact: true }).fill(adjustmentAmount)
    await form.getByLabel('调整原因', { exact: true }).fill(adjustmentReason)
    const responsePromise = waitForResponseSafely(page, (response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${projectId}/charges/${original.id}/adjustments`
    })
    await form.getByRole('button', { name: '记录调整', exact: true }).click()
    try {
      await expectJson(await responsePromise, 'REAL_WORK_FINANCE_ADJUSTMENT_FAILED')
    } catch (error) {
      const observed = await readRealWorkFinanceSnapshot(page.request, projectId)
      const observedAdjustment = observed.charges.find((charge) => charge?.adjustmentOf === original.id)
      if (!observedAdjustment) throw error
    }

    const adjusted = await waitForRealWorkAdjustment(page.request, projectId, original, expectedSpent, adjustmentReason)
    await page.reload({ waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('heading', { name: '费率、预算与费用', exact: true })).toBeVisible({ timeout: 120_000 })
    await expect(page.getByTestId('budget-threshold-warning')).toHaveCount(0)
    await expect(page.getByTestId('budget-limit-warning')).toHaveCount(0)
    const originalRowAfterAdjustment = realWorkChargeRow(page, original.id)
    await expect(originalRowAfterAdjustment).toHaveCount(1, { timeout: 120_000 })
    await expect(originalRowAfterAdjustment.locator('.charge-main > strong')).toContainText(
      `${original.total.amount} ${original.total.currency}`,
    )
    const adjustmentUiRow = page.locator('.charge-row').filter({ hasText: '调整原因：试用账目调整' })
    await expect(adjustmentUiRow).toHaveCount(1, { timeout: 120_000 })

    const restored = await saveRealWorkBudgetByUi(
      page,
      projectId,
      initial.budget.limit.amount,
      initial.budget.warningAt.amount,
    )
    budgetRestored = true
    const finalSnapshot = await readRealWorkFinanceSnapshot(page.request, projectId)
    if (
      finalSnapshot.budget.limit.amount !== initial.budget.limit.amount
      || finalSnapshot.budget.warningAt.amount !== initial.budget.warningAt.amount
      || finalSnapshot.budget.spent.amount !== expectedSpent
    ) throw new Error('REAL_WORK_FINANCE_BUDGET_RESTORE_READBACK_MISMATCH')
    await page.reload({ waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('heading', { name: '费率、预算与费用', exact: true })).toBeVisible({ timeout: 120_000 })
    await expect(page.locator('.budget-summary')).toBeVisible({ timeout: 120_000 })
    await expect(page.locator('.budget-form input').nth(2)).toHaveValue(initial.budget.warningAt.amount)
    await expect(realWorkChargeRow(page, original.id)).toHaveCount(1, { timeout: 120_000 })
    await expect(page.locator('.charge-row').filter({ hasText: '调整原因：试用账目调整' })).toHaveCount(1, { timeout: 120_000 })
    result = {
      originalCharge: original,
      adjustment: adjusted.adjustment,
      budgetBefore: initial.budget,
      budgetAfterAdjustment: adjusted.snapshot.budget,
      budgetRestored: restored.budget,
    }
  } catch (error) {
    failure = error
  }

  if (budgetOverrideApplied && !budgetRestored) {
    try {
      if (!initialBudget) throw new Error('REAL_WORK_FINANCE_BUDGET_BASELINE_MISSING')
      await saveRealWorkBudgetByUi(page, projectId, initialBudget.limit.amount, initialBudget.warningAt.amount)
      budgetRestored = true
    } catch (restoreError) {
      failure = failure
        ? new AggregateError([failure, restoreError], 'REAL_WORK_FINANCE_BUDGET_RESTORE_FAILED')
        : restoreError
    }
  }
  await context.close()
  if (failure) throw failure
  return result
}

export async function waitForDeletedEnvironment(request, environmentId) {
  let latest
  await expect.poll(async () => {
    const response = await request.get(`/api/v1/environments/${environmentId}`)
    if (response.status() === 404) return true
    latest = await expectJson(response, 'ENVIRONMENT_CLEANUP_READ_FAILED')
    return latest.observedState === 'deleted'
  }, { timeout: 240_000, intervals: [1000, 2000, 3000] }).toBe(true)
  return latest
}

export async function cleanupWorkResources(request, _baseURL, projectId, environmentId, leaseId, requestId, uiPage) {
  if (!uiPage) throw new Error('REAL_WORK_CLEANUP_UI_PAGE_REQUIRED')
  const failures = []
  let releaseWaitFailure = null
  let claimId = null
  try {
    if (!leaseId && requestId) {
      const leases = await expectJson(
        await request.get(`/api/v1/projects/${projectId}/resource-leases`),
        'REAL_WORK_CLEANUP_LEASE_LIST_FAILED',
      )
      if (!Array.isArray(leases)) throw new Error('REAL_WORK_CLEANUP_LEASE_LIST_INVALID')
      leaseId = leases.find((lease) => lease.requestId === requestId)?.id ?? null

      if (!leaseId) {
        let trackedRequest = await expectJson(
          await request.get(`/api/v1/resource-requests/${requestId}`),
          'REAL_WORK_CLEANUP_RESOURCE_REQUEST_READ_FAILED',
        )
        if (trackedRequest.state === 'reviewing') {
          await cancelProjectResourceRequestByUi(uiPage, { projectId, requestKey: trackedRequest.requestKey })
          trackedRequest = await pollJson(
            request,
            `/api/v1/resource-requests/${requestId}`,
            (value) => ['rejected', 'cancelled'].includes(value.state),
            'REAL_WORK_CLEANUP_RESOURCE_REQUEST_CANCEL_STATUS_FAILED',
            120_000,
          )
          if (trackedRequest.state !== 'cancelled') {
            throw new Error(`REAL_WORK_CLEANUP_RESOURCE_REQUEST_NOT_CANCELLED:${trackedRequest.state}`)
          }
        } else if (trackedRequest.state === 'allocating') {
          trackedRequest = await pollJson(
            request,
            `/api/v1/resource-requests/${requestId}`,
            (value) => value.state !== 'allocating',
            'REAL_WORK_CLEANUP_RESOURCE_REQUEST_ALLOCATION_STATUS_FAILED',
            120_000,
          )
          if (trackedRequest.state === 'active' || trackedRequest.state === 'expiring') {
            const allocatedLeases = await expectJson(
              await request.get(`/api/v1/projects/${projectId}/resource-leases`),
              'REAL_WORK_CLEANUP_LEASE_LIST_AFTER_ALLOCATION_FAILED',
            )
            if (!Array.isArray(allocatedLeases)) throw new Error('REAL_WORK_CLEANUP_LEASE_LIST_AFTER_ALLOCATION_INVALID')
            leaseId = allocatedLeases.find((lease) => lease.requestId === requestId)?.id ?? null
            if (!leaseId) throw new Error(`REAL_WORK_CLEANUP_LEASE_MISSING:${trackedRequest.state}`)
          }
        } else if (trackedRequest.state === 'active' || trackedRequest.state === 'expiring') {
          // A lease can be projected just after the request reaches its active
          // state. Re-read the authoritative project list before deciding that
          // cleanup is unsafe; never release an untracked lease by guessing.
          const activeLeases = await expectJson(
            await request.get(`/api/v1/projects/${projectId}/resource-leases`),
            'REAL_WORK_CLEANUP_LEASE_LIST_AFTER_ACTIVE_FAILED',
          )
          if (!Array.isArray(activeLeases)) throw new Error('REAL_WORK_CLEANUP_LEASE_LIST_AFTER_ACTIVE_INVALID')
          leaseId = activeLeases.find((lease) => lease.requestId === requestId)?.id ?? null
          if (!leaseId) throw new Error(`REAL_WORK_CLEANUP_LEASE_MISSING:${trackedRequest.state}`)
        }
        if (!leaseId && !['rejected', 'cancelled', 'expired'].includes(trackedRequest.state)) {
          throw new Error(`REAL_WORK_CLEANUP_RESOURCE_REQUEST_UNSAFE_WITHOUT_LEASE:${trackedRequest.state}`)
        }
      }
    }
  } catch (error) {
    failures.push(error)
  }

  try {
    if (environmentId) {
      const currentResponse = await request.get(`/api/v1/environments/${environmentId}`)
      if (currentResponse.status() !== 404) {
        let current = await expectJson(currentResponse, 'REAL_WORK_CLEANUP_ENVIRONMENT_READ_FAILED')
        expect(current).toMatchObject({ id: environmentId, projectId })
        if (current.observedState !== 'deleted') {
          if (['requested', 'validating', 'building', 'provisioning', 'updating'].includes(current.observedState)) {
            current = await pollJson(
              request,
              `/api/v1/environments/${environmentId}`,
              (value) => ['ready', 'stopped', 'failed', 'deleting', 'deleted'].includes(value.observedState),
              'REAL_WORK_CLEANUP_ENVIRONMENT_PROVISION_FAILED',
              240_000,
            )
          }
          if (current.observedState === 'ready') {
            current = await stopEnvironmentByUi(uiPage, {
              routePrefix: 'student',
              projectId,
              environmentId,
              label: 'REAL_WORK_CLEANUP_ENVIRONMENT_STOP',
            })
          }
          if (current.observedState === 'deleting') {
            await waitForDeletedEnvironment(request, environmentId)
          }
        }
      }
    }
  } catch (error) {
    failures.push(error)
  }

  try {
    if (leaseId) {
      const leaseResponse = await request.get(`/api/v1/resource-leases/${leaseId}`)
      const lease = await expectJson(leaseResponse, 'REAL_WORK_CLEANUP_LEASE_READ_FAILED')
      expect(lease).toMatchObject({ id: leaseId, ...(requestId ? { requestId } : {}) })
      claimId = lease.claimId
      if (['active', 'allocating'].includes(lease.state)) {
        await releaseProjectLeaseByUi(uiPage, { projectId, requestId: requestId ?? lease.requestId, leaseId })
      }
      if (['active', 'allocating', 'expiring'].includes(lease.state)) {
        try {
          const settled = await pollJson(
            request,
            `/api/v1/resource-leases/${leaseId}`,
            async (value) => {
              if (['revoked', 'expired'].includes(value.state)) return true
              if (!environmentId) return false
              const environment = await expectJson(
                await request.get(`/api/v1/environments/${environmentId}`),
                'REAL_WORK_CLEANUP_ENVIRONMENT_RELEASE_READ_FAILED',
              )
              expect(environment).toMatchObject({ id: environmentId, projectId })
              return environment.desiredState === 'deleted'
                && environment.observedState === 'failed'
                && environment.operation?.state === 'failed'
            },
            'REAL_WORK_CLEANUP_LEASE_REVOKE_STATUS_FAILED',
            240_000,
          )
          if (!['revoked', 'expired'].includes(settled.state)) {
            throw new Error('REAL_WORK_CLEANUP_RESOURCE_ENVIRONMENT_OPERATION_FAILED')
          }
        } catch (error) {
          // Keep the diagnostic while attempting the owner's normal delete path.
          // Release is successful only after the final Resource readback below.
          if (/REAL_WORK_CLEANUP_(LEASE_REVOKE_STATUS|ENVIRONMENT_RELEASE_READ)_FAILED:/.test(error.message)) {
            failures.push(error)
          } else {
            releaseWaitFailure = error
          }
        }
      } else if (!['revoked', 'expired'].includes(lease.state)) {
        throw new Error(`REAL_WORK_CLEANUP_LEASE_STATE_INVALID:${lease.state}`)
      }
    }
  } catch (error) {
    failures.push(error)
  }

  try {
    if (environmentId) {
      const latestResponse = await request.get(`/api/v1/environments/${environmentId}`)
      if (latestResponse.status() !== 404) {
        const latest = await expectJson(latestResponse, 'REAL_WORK_CLEANUP_ENVIRONMENT_READ_BEFORE_DELETE_FAILED')
        expect(latest).toMatchObject({ id: environmentId, projectId })
        if (!['deleting', 'deleted'].includes(latest.observedState)) {
          await deleteEnvironmentByUi(uiPage, {
            routePrefix: 'student',
            projectId,
            environmentId,
            label: 'REAL_WORK_CLEANUP_ENVIRONMENT_DELETE',
          })
        }
        await waitForDeletedEnvironment(request, environmentId)
      }
    }
  } catch (error) {
    failures.push(error)
  }

  try {
    if (leaseId) {
      const finalLease = await pollJson(
        request,
        `/api/v1/resource-leases/${leaseId}`,
        (value) => ['revoked', 'expired'].includes(value.state),
        'REAL_WORK_CLEANUP_LEASE_FINAL_STATUS_FAILED',
        240_000,
      )
      expect(finalLease).toMatchObject({ id: leaseId, ...(claimId ? { claimId } : {}), ...(requestId ? { requestId } : {}) })
    }
    if (requestId) {
      const finalRequest = await pollJson(
        request,
        `/api/v1/resource-requests/${requestId}`,
        (value) => leaseId ? value.state === 'expired' : ['rejected', 'cancelled', 'expired'].includes(value.state),
        'REAL_WORK_CLEANUP_RESOURCE_REQUEST_FINAL_STATUS_FAILED',
        240_000,
      )
      expect(finalRequest).toMatchObject({ id: requestId, projectId })
    }
  } catch (error) {
    failures.push(error)
  }
  if (failures.length > 0) {
    if (releaseWaitFailure) failures.push(releaseWaitFailure)
    throw new AggregateError(failures, `REAL_WORK_CLEANUP_FAILED:${failures.map((error) => error.message).join(';')}`)
  }
  if (releaseWaitFailure) console.warn('REAL_WORK_CLEANUP_RECOVERED:RESOURCE_RELEASE_WAIT_FAILED')
}
