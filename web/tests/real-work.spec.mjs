import { readFile, readdir } from 'node:fs/promises'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const cleanupUi = vi.hoisted(() => ({
  cancelRequest: vi.fn(),
  stopEnvironment: vi.fn(),
  releaseLease: vi.fn(),
  deleteEnvironment: vi.fn(),
}))

vi.mock('../e2e/support/environment-lifecycle.mjs', () => ({
  stopEnvironmentByUi: cleanupUi.stopEnvironment,
  deleteEnvironmentByUi: cleanupUi.deleteEnvironment,
}))
vi.mock('../e2e/support/real-resource.mjs', () => ({
  cancelProjectResourceRequestByUi: cleanupUi.cancelRequest,
  releaseProjectLeaseByUi: cleanupUi.releaseLease,
}))

import {
  assertRealWorkVmCandidate,
  assertRealWorkGpuContainerCandidate,
  createRealWorkPackage,
  cleanupWorkResources,
  DEFAULT_RATE_INPUTS,
  ensureRateByUi,
  realWorkConfig,
  realWorkGpuConfig,
  realWorkVmConfig,
  readProjectUsage,
  readResumablePublishedWork,
  selectSettledExperimentUsageCharges,
  selectPendingWorkTaskResourceRequest,
  selectSettledWorkUsageChargesForLease,
  verifyRealWorkFinanceAdjustmentByUi,
  waitForActiveRateReadback,
} from '../e2e/support/real-work.mjs'

const GIB = 1024 ** 3
describe('Work authoring resource approval selection', () => {
  const runId = '00000000-0000-7000-8000-000000000001'
  const taskId = '00000000-0000-7000-8000-000000000002'
  const otherTaskId = '00000000-0000-7000-8000-000000000003'
  const scope = { projectId: 'project', runId, trackKind: 'environment', attemptNumber: 1, studentActorId: 'student' }
  function taskRequest({ task = taskId, track = 'environment', attempt = 1, run = runId, ...overrides } = {}) {
    return {
      id: task,
      projectId: scope.projectId,
      requesterId: scope.studentActorId,
      requestKey: `authoring-${run.replaceAll('-', '')}-${track}-${attempt}-${task.replaceAll('-', '')}`,
      target: { kind: 'task', taskRunId: task },
      state: 'reviewing',
      requestedResources: { cpuMillicores: 1000, memoryBytes: GIB, storageBytes: GIB, gpu: null },
      ...overrides,
    }
  }

  it('selects a schema repair request after the same attempt task has expired', () => {
    const repaired = taskRequest({ task: otherTaskId })
    expect(selectPendingWorkTaskResourceRequest([taskRequest({ state: 'expired' }), repaired], scope)).toBe(repaired)
  })

  it('selects only the requested track when both authoring tracks need approval', () => {
    const environment = taskRequest()
    const evaluation = taskRequest({ task: otherTaskId, track: 'evaluation' })
    expect(selectPendingWorkTaskResourceRequest([evaluation, environment], scope)).toBe(environment)
    expect(selectPendingWorkTaskResourceRequest([evaluation, environment], { ...scope, trackKind: 'evaluation' })).toBe(evaluation)
  })

  it.each(['reviewing', 'allocating', 'active', 'expiring'])('rejects two effective requests when one is %s', (state) => {
    expect(() => selectPendingWorkTaskResourceRequest([taskRequest({ state }), taskRequest({ task: otherTaskId })], scope))
      .toThrow('WORK_TASK_RESOURCE_REQUEST_DUPLICATE')
  })

  it.each(['expired', 'rejected', 'cancelled'])('does not select terminal history %s', (state) => {
    expect(selectPendingWorkTaskResourceRequest([taskRequest({ state })], scope)).toBeNull()
  })

  it('does not re-approve a task lease already approved in this run poll', () => {
    const request = taskRequest()
    expect(selectPendingWorkTaskResourceRequest([request], {
      ...scope,
      ignoredRequestIds: new Set([request.id]),
    })).toBeNull()
  })

  it('does not approve another run, track, or attempt', () => {
    expect(selectPendingWorkTaskResourceRequest([
      taskRequest({ run: otherTaskId }),
      taskRequest({ track: 'evaluation' }),
      taskRequest({ attempt: 2 }),
    ], scope)).toBeNull()
  })

  it.each([
    { projectId: 'foreign' },
    { requesterId: 'foreign' },
    { target: { kind: 'environment', environmentId: taskId } },
    { target: { kind: 'task', taskRunId: otherTaskId } },
    { requestedResources: { cpuMillicores: 0, memoryBytes: GIB, storageBytes: GIB } },
    { requestedResources: { cpuMillicores: 1000, memoryBytes: GIB, storageBytes: GIB, gpu: { class: 'nvidia-cuda', count: 1 } } },
    { state: 'revoked' },
    { state: 'unknown' },
  ])('rejects invalid same-track request scope %#', (overrides) => {
    expect(() => selectPendingWorkTaskResourceRequest([taskRequest(overrides)], scope))
      .toThrow('WORK_TASK_RESOURCE_REQUEST_SCOPE_INVALID')
  })

  it('validates terminal history before excluding it', () => {
    expect(() => selectPendingWorkTaskResourceRequest([taskRequest({ state: 'expired', requesterId: 'foreign' })], scope))
      .toThrow('WORK_TASK_RESOURCE_REQUEST_SCOPE_INVALID')
  })

  it('rejects malformed identity under the current run prefix', () => {
    const request = taskRequest({ requestKey: `authoring-${runId.replaceAll('-', '')}-environment-bad` })
    expect(() => selectPendingWorkTaskResourceRequest([request], scope)).toThrow('WORK_TASK_RESOURCE_REQUEST_SCOPE_INVALID')
  })
})

describe('Work usage and charge association', () => {
  const scope = { projectId: 'project', requestId: 'request', leaseId: 'lease' }
  const rates = Object.freeze({
    cpu: Object.freeze({ id: 'cpu-rate', revision: 1, unit: 'cpu_millicore_second', unitQuantity: 1, unitPrice: { amount: '0.000001', currency: 'USD' }, effectiveFrom: '2026-10-04T00:00:00.000Z', effectiveUntil: null }),
    memory: Object.freeze({ id: 'memory-rate', revision: 1, unit: 'memory_byte_second', unitQuantity: 1, unitPrice: { amount: '0.000001', currency: 'USD' }, effectiveFrom: '2026-10-04T00:00:00.000Z', effectiveUntil: null }),
    storage: Object.freeze({ id: 'storage-rate', revision: 1, unit: 'storage_byte_second', unitQuantity: 1, unitPrice: { amount: '0.000001', currency: 'USD' }, effectiveFrom: '2026-10-04T00:00:00.000Z', effectiveUntil: null }),
    gpu: Object.freeze({ id: 'gpu-rate', revision: 1, unit: 'gpu_unit_second', unitQuantity: 1, gpuClass: 'nvidia-a100', gpuMode: 'exclusive', unitPrice: { amount: '0.010000', currency: 'USD' }, effectiveFrom: '2026-10-04T00:00:00.000Z', effectiveUntil: null }),
  })
  const chargeLine = (rate, quantity, amount) => ({
    rateId: rate.id,
    rateRevision: rate.revision,
    unit: rate.unit,
    quantity,
    unitQuantity: rate.unitQuantity,
    unitPrice: rate.unitPrice,
    amount: { amount, currency: rate.unitPrice.currency },
  })
  const usage = (kind, overrides = {}) => ({
    id: `usage-${kind}`,
    projectId: scope.projectId,
    target: {
      kind: 'resource_request',
      requestId: scope.requestId,
      leaseId: scope.leaseId,
    },
    measuredFrom: '2026-10-04T05:00:00.000Z',
    measuredUntil: '2026-10-04T05:01:00.000Z',
    kind,
    settlement: 'settled',
    measurement: {
      state: 'known',
      quantities: kind === 'compute'
        ? { cpuMillicoreSeconds: 10, memoryByteSeconds: 20, gpuUnitSeconds: 0, storageByteSeconds: 0 }
        : { cpuMillicoreSeconds: 0, memoryByteSeconds: 0, gpuUnitSeconds: 0, storageByteSeconds: 30 },
    },
    ...overrides,
  })
  const charge = (usageRecordId, kind, overrides = {}) => ({
    id: `charge-${kind}`,
    projectId: scope.projectId,
    usageRecordId,
    adjustmentOf: null,
    settlement: 'settled',
    total: { amount: '0.000030', currency: 'USD' },
    lines: kind === 'compute'
      ? [
          chargeLine(rates.cpu, 10, '0.000010'),
          chargeLine(rates.memory, 20, '0.000020'),
        ]
      : [chargeLine(rates.storage, 30, '0.000030')],
    ...overrides,
  })
  const experimentUsage = (kind, { gpu = false, ...overrides } = {}) => ({
    id: `experiment-usage-${kind}`,
    projectId: scope.projectId,
    target: { kind: 'experiment_environment', environmentId: 'environment' },
    measuredFrom: '2026-10-04T05:00:00.000Z',
    measuredUntil: '2026-10-04T05:01:00.000Z',
    kind,
    settlement: 'settled',
    measurement: {
      state: 'known',
      quantities: kind === 'compute'
        ? { cpuMillicoreSeconds: 10, memoryByteSeconds: 20, gpuUnitSeconds: gpu ? 10 : 0, storageByteSeconds: 0 }
        : { cpuMillicoreSeconds: 0, memoryByteSeconds: 0, gpuUnitSeconds: 0, storageByteSeconds: 30 },
    },
    ...overrides,
  })
  const experimentCharge = (usageRecordId, kind, gpu = false) => ({
    id: `experiment-charge-${kind}`,
    projectId: scope.projectId,
    usageRecordId,
    adjustmentOf: null,
    settlement: 'settled',
    total: { amount: gpu ? '0.100030' : '0.000030', currency: 'USD' },
    lines: kind === 'compute'
      ? [
          chargeLine(rates.cpu, 10, '0.000010'),
          chargeLine(rates.memory, 20, '0.000020'),
          ...(gpu ? [chargeLine(rates.gpu, 10, '0.100000')] : []),
        ]
      : [chargeLine(rates.storage, 30, '0.000030')],
  })

  it('requires this lease usage instead of accepting an older positive project charge', () => {
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [usage('compute'), usage('storage')],
      charges: [
        charge('usage-compute', 'compute', { id: 'charge-old-compute' }),
        charge('usage-storage', 'storage', { id: 'charge-old-storage' }),
      ],
      baselineChargeIds: new Set(['charge-old-compute', 'charge-old-storage']),
    })
    expect(result).toBeNull()
  })

  it('matches known settled compute and storage records by request and lease', () => {
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [usage('compute'), usage('storage')],
      charges: [charge('usage-compute', 'compute'), charge('usage-storage', 'storage')],
      baselineChargeIds: new Set(),
    })
    expect(result?.map(({ usage: item, charge: itemCharge }) => [item.kind, itemCharge.usageRecordId])).toEqual([
      ['compute', 'usage-compute'],
      ['storage', 'usage-storage'],
    ])
  })

  it('accepts a settled zero-total charge when a zero-priced rate matches the usage', () => {
    const zeroCharge = (usageRecordId, kind) => {
      const item = charge(usageRecordId, kind)
      return {
        ...item,
        total: { amount: '0.000000', currency: 'USD' },
        lines: item.lines.map((line) => ({
          ...line,
          rateId: `${line.rateId}-zero`,
          rateRevision: line.rateRevision + 1,
          unitPrice: { ...line.unitPrice, amount: '0.000000' },
          amount: { ...line.amount, amount: '0.000000' },
        })),
      }
    }
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [usage('compute'), usage('storage')],
      charges: [zeroCharge('usage-compute', 'compute'), zeroCharge('usage-storage', 'storage')],
      baselineChargeIds: new Set(),
    })
    expect(result?.map(({ usage: item }) => item.kind)).toEqual(['compute', 'storage'])
  })

  it.each([
    { settlement: 'pending' },
    { measurement: { state: 'unknown', reason: 'meter unavailable' } },
  ])('does not complete while usage is not known and settled: %o', (overrides) => {
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [usage('compute', overrides), usage('storage')],
      charges: [charge('usage-compute', 'compute'), charge('usage-storage', 'storage')],
      baselineChargeIds: new Set(),
    })
    expect(result).toBeNull()
  })

  it.each([
    ['unknown', { id: 'usage-extra-unknown', measurement: { state: 'unknown', reason: 'meter unavailable' } }],
    ['pending', { id: 'usage-extra-pending', settlement: 'pending' }],
  ])('blocks an extra same-lease %s usage record', (_state, overrides) => {
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [usage('compute'), usage('storage'), usage('compute', overrides)],
      charges: [charge('usage-compute', 'compute'), charge('usage-storage', 'storage')],
      baselineChargeIds: new Set(),
    })
    expect(result).toBeNull()
  })

  it('blocks a usage record with a missing measured interval', () => {
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [
        usage('compute', { measuredUntil: undefined }),
        usage('storage'),
      ],
      charges: [charge('usage-compute', 'compute'), charge('usage-storage', 'storage')],
      baselineChargeIds: new Set(),
    })
    expect(result).toBeNull()
  })

  it('requires every positive compute dimension to have matching charge quantity', () => {
    const computeCharge = charge('usage-compute', 'compute', {
      total: { amount: '0.000010', currency: 'USD' },
      lines: [chargeLine(rates.cpu, 10, '0.000010')],
    })
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [usage('compute'), usage('storage')],
      charges: [computeCharge, charge('usage-storage', 'storage')],
      baselineChargeIds: new Set(),
    })
    expect(result).toBeNull()
  })

  it('blocks a known usage with a missing memory quantity', () => {
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [
        usage('compute', {
          measurement: {
            state: 'known',
            quantities: { cpuMillicoreSeconds: 10, storageByteSeconds: 0, gpuUnitSeconds: 0 },
          },
        }),
        usage('storage'),
      ],
      charges: [charge('usage-compute', 'compute'), charge('usage-storage', 'storage')],
      baselineChargeIds: new Set(),
    })
    expect(result).toBeNull()
  })

  it('sums segmented charge lines for every positive usage dimension', () => {
    const computeCharge = charge('usage-compute', 'compute', {
      lines: [
        chargeLine(rates.cpu, 4, '0.000004'),
        chargeLine(rates.cpu, 6, '0.000006'),
        chargeLine(rates.memory, 10, '0.000010'),
        chargeLine(rates.memory, 10, '0.000010'),
      ],
      total: { amount: '0.000030', currency: 'USD' },
    })
    const storageCharge = charge('usage-storage', 'storage', {
      lines: [
        chargeLine(rates.storage, 10, '0.000010'),
        chargeLine(rates.storage, 20, '0.000020'),
      ],
      total: { amount: '0.000030', currency: 'USD' },
    })
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [usage('compute'), usage('storage')],
      charges: [computeCharge, storageCharge],
      baselineChargeIds: new Set(),
    })
    expect(result?.map(({ usage: item }) => item.kind)).toEqual(['compute', 'storage'])
  })

  it.each([
    ['storage', chargeLine(rates.storage, 1, '0.000001')],
    ['zero-gpu', chargeLine(rates.gpu, 1, '0.010000')],
  ])('rejects an extra positive %s charge line outside the usage dimensions', (_kind, extraLine) => {
    const baseComputeCharge = charge('usage-compute', 'compute')
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [usage('compute'), usage('storage')],
      charges: [
        {
          ...baseComputeCharge,
          total: { amount: extraLine.unit === 'gpu_unit_second' ? '0.010030' : '0.000031', currency: 'USD' },
          lines: [...baseComputeCharge.lines, extraLine],
        },
        charge('usage-storage', 'storage'),
      ],
      baselineChargeIds: new Set(),
    })
    expect(result).toBeNull()
  })

  it('does not match usage or charges from another request or lease', () => {
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [
        usage('compute', { target: { kind: 'resource_request', requestId: 'other-request', leaseId: scope.leaseId } }),
        usage('storage', { target: { kind: 'resource_request', requestId: scope.requestId, leaseId: 'other-lease' } }),
      ],
      charges: [charge('usage-compute', 'compute'), charge('usage-storage', 'storage')],
      baselineChargeIds: new Set(),
    })
    expect(result).toBeNull()
  })

  it('requires the resource request target instead of legacy top-level identities or experiment targets', () => {
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [
        {
          ...usage('compute'),
          target: undefined,
          requestId: scope.requestId,
          leaseId: scope.leaseId,
        },
        usage('storage', { target: { kind: 'experiment_environment', environmentId: 'environment-1' } }),
      ],
      charges: [charge('usage-compute', 'compute'), charge('usage-storage', 'storage')],
      baselineChargeIds: new Set(),
    })
    expect(result).toBeNull()
  })

  it('reads every project usage page before matching a lease', async () => {
    const pages = new Map([
      [1, { items: [{ id: 'usage-1' }], page: 1, pageSize: 2, hasMore: true }],
      [2, { items: [{ id: 'usage-2' }], page: 2, pageSize: 2, hasMore: false }],
    ])
    const paths = []
    const request = {
      get: vi.fn(async (path) => {
        paths.push(path)
        const page = Number(new URL(`https://portal.invalid${path}`).searchParams.get('page'))
        return {
          ok: () => true,
          status: () => 200,
          text: async () => JSON.stringify(pages.get(page)),
        }
      }),
    }
    await expect(readProjectUsage(request, 'project', { pageSize: 2 })).resolves.toEqual([
      { id: 'usage-1' },
      { id: 'usage-2' },
    ])
    expect(paths).toEqual([
      '/api/v1/projects/project/usage?page=1&pageSize=2',
      '/api/v1/projects/project/usage?page=2&pageSize=2',
    ])
  })

  it('requires the settled GPU line to use the requested class, mode, revision and unit price', () => {
    const gpuRate = { ...rates.gpu, revision: 3 }
    const gpuUsage = usage('compute', {
      measurement: {
        state: 'known',
        quantities: { cpuMillicoreSeconds: 0, memoryByteSeconds: 0, gpuUnitSeconds: 10, storageByteSeconds: 0 },
      },
    })
    const gpuCharge = charge('usage-compute', 'compute', {
      total: { amount: '0.100000', currency: 'USD' },
      lines: [{
        rateId: gpuRate.id,
        rateRevision: gpuRate.revision,
        unit: 'gpu_unit_second',
        quantity: 10,
        unitQuantity: gpuRate.unitQuantity,
        unitPrice: gpuRate.unitPrice,
        amount: { amount: '0.100000', currency: 'USD' },
      }],
    })
    const result = selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [gpuUsage, usage('storage')],
      charges: [gpuCharge, charge('usage-storage', 'storage')],
      baselineChargeIds: new Set(),
      gpu: { class: gpuRate.gpuClass, mode: gpuRate.gpuMode, rate: gpuRate },
    })
    expect(result?.map(({ usage: item }) => item.kind)).toEqual(['compute', 'storage'])

    expect(selectSettledWorkUsageChargesForLease({
      ...scope,
      usageRecords: [gpuUsage, usage('storage')],
      charges: [
        { ...gpuCharge, lines: [{ ...gpuCharge.lines[0], rateRevision: 2 }] },
        charge('usage-storage', 'storage'),
      ],
      baselineChargeIds: new Set(),
      gpu: { class: gpuRate.gpuClass, mode: gpuRate.gpuMode, rate: gpuRate },
    })).toBeNull()
  })

  it('matches experiment environment usage by target and exact rate-window charges', () => {
    const result = selectSettledExperimentUsageCharges({
      ...scope,
      environmentId: 'environment',
      usageRecords: [experimentUsage('storage'), experimentUsage('compute', { gpu: true })],
      charges: [experimentCharge('experiment-usage-compute', 'compute', true), experimentCharge('experiment-usage-storage', 'storage')],
      baselineChargeIds: new Set(),
      gpu: { class: rates.gpu.gpuClass, mode: rates.gpu.gpuMode },
      rates: Object.values(rates),
    })
    expect(result?.map(({ usage: item }) => item.kind)).toEqual(['storage', 'compute'])
  })

  it('does not accept another environment or a baseline charge for this environment', () => {
    const usageRecords = [
      experimentUsage('compute', { target: { kind: 'experiment_environment', environmentId: 'other-environment' } }),
      experimentUsage('storage'),
    ]
    expect(selectSettledExperimentUsageCharges({
      ...scope,
      environmentId: 'environment',
      usageRecords,
      charges: [experimentCharge('experiment-usage-compute', 'compute'), experimentCharge('experiment-usage-storage', 'storage')],
      baselineChargeIds: new Set(),
      rates: Object.values(rates),
    })).toBeNull()
    expect(selectSettledExperimentUsageCharges({
      ...scope,
      environmentId: 'environment',
      usageRecords: [experimentUsage('compute'), experimentUsage('storage')],
      charges: [experimentCharge('experiment-usage-compute', 'compute'), experimentCharge('experiment-usage-storage', 'storage')],
      baselineChargeIds: new Set(['experiment-charge-compute']),
      rates: Object.values(rates),
    })).toBeNull()
  })

  it('does not complete while teaching usage is pending or duplicated', () => {
    expect(selectSettledExperimentUsageCharges({
      ...scope,
      environmentId: 'environment',
      usageRecords: [experimentUsage('compute', { settlement: 'pending' }), experimentUsage('storage')],
      charges: [experimentCharge('experiment-usage-compute', 'compute'), experimentCharge('experiment-usage-storage', 'storage')],
      baselineChargeIds: new Set(),
      rates: Object.values(rates),
    })).toBeNull()
    expect(() => selectSettledExperimentUsageCharges({
      ...scope,
      environmentId: 'environment',
      usageRecords: [experimentUsage('compute'), experimentUsage('compute'), experimentUsage('storage')],
      charges: [experimentCharge('experiment-usage-compute', 'compute'), experimentCharge('experiment-usage-storage', 'storage')],
      baselineChargeIds: new Set(),
      rates: Object.values(rates),
    })).toThrow('LW_EXPERIMENT_USAGE_DUPLICATE')
  })
})

describe('Work acceptance demonstration rates', () => {
  const now = Date.parse('2026-10-04T06:00:00Z')
  const context = (rates) => ({ request: { get: vi.fn(async () => ({ ok: () => true, text: async () => JSON.stringify(rates) })) } })
  const rate = (target, overrides = {}) => ({
    id: 'operator-rate', revision: 1, unit: target.unit, unitQuantity: target.unitQuantity,
    gpuClass: null, gpuMode: null, unitPrice: { amount: '7.000000', currency: 'USD' },
    effectiveFrom: '2026-10-04T05:00:00Z', effectiveUntil: null, ...overrides,
  })

  beforeEach(() => vi.spyOn(Date, 'now').mockReturnValue(now))
  afterEach(() => vi.restoreAllMocks())

  it.each([
    ['cpu_millicore_second', 1000 * 3600],
    ['memory_byte_second', GIB * 3600],
    ['storage_byte_second', GIB * 3600],
  ])('uses one human resource-hour as the %s demonstration price quantity', (unit, quantity) => {
    const target = DEFAULT_RATE_INPUTS.find((item) => item.unit === unit)
    expect(target).toBeDefined()
    expect(Number.isSafeInteger(target.unitQuantity)).toBe(true)
    expect(quantity / target.unitQuantity * Number(target.amount)).toBe(1)
    expect(target.currency).toBe('USD')
  })

  it('keeps the current operator price when a later version is scheduled', async () => {
    const target = DEFAULT_RATE_INPUTS[1]
    const current = rate(target, { unitQuantity: 1_000_000, effectiveUntil: '2026-10-05T00:00:00Z' })
    const future = rate(target, { id: 'future', revision: 2, effectiveFrom: current.effectiveUntil })
    const page = { getByTestId: vi.fn() }
    await expect(ensureRateByUi(page, context([future, current]), target)).resolves.toEqual(current)
    expect(page.getByTestId).not.toHaveBeenCalled()
  })

  it('does not insert a backdated demonstration price before a scheduled operator rate', async () => {
    const target = DEFAULT_RATE_INPUTS[0]
    const page = { getByTestId: vi.fn() }
    await expect(ensureRateByUi(page, context([rate(target, { effectiveFrom: '2026-10-05T00:00:00Z' })]), target))
      .rejects.toThrow('REAL_WORK_RATE_FUTURE_CONFIGURED:cpu_millicore_second')
    expect(page.getByTestId).not.toHaveBeenCalled()
  })

  it('waits for a newly created future rate to become active before reading it back', async () => {
    const target = DEFAULT_RATE_INPUTS[0]
    const future = rate(target, { effectiveFrom: '2026-10-04T06:01:00Z' })
    const active = rate(target, { id: 'active', effectiveFrom: '2026-10-04T06:00:00Z' })
    let reads = 0
    const context = {
      request: {
        get: vi.fn(async () => ({
          ok: () => true,
          text: async () => JSON.stringify(reads++ === 0 ? [future] : [active]),
        })),
      },
    }
    await expect(waitForActiveRateReadback(context, target)).resolves.toEqual(active)
    expect(context.request.get).toHaveBeenCalledTimes(2)
  })

  it.each([0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1])('rejects a configured unsafe rate quantity %s before a create action', async (quantity) => {
    const target = DEFAULT_RATE_INPUTS[0]
    const page = { getByTestId: vi.fn() }
    await expect(ensureRateByUi(page, context([rate(target, { unitQuantity: quantity })]), target))
      .rejects.toThrow('REAL_WORK_RATE_QUANTITY_INVALID')
    expect(page.getByTestId).not.toHaveBeenCalled()
  })

  it('keeps the explicit GPU price conflict guard', async () => {
    const target = { unit: 'gpu_unit_second', unitQuantity: 1, amount: '0.000100', currency: 'USD', gpuClass: 'nvidia-cuda', gpuMode: 'exclusive' }
    await expect(ensureRateByUi({}, context([rate(target, { gpuClass: target.gpuClass, gpuMode: target.gpuMode })]), target))
      .rejects.toThrow('REAL_WORK_GPU_RATE_ACTIVE_CONFLICT:nvidia-cuda')
  })

  it('resolves a bounded container-time-slice trial rate for the current finance form', () => {
    process.env.LABWEAVER_E2E_WORK_GPU_CLASS = 'nvidia-cuda'
    process.env.LABWEAVER_E2E_WORK_GPU_MODE = 'container_time_slice'
    process.env.LABWEAVER_E2E_WORK_GPU_RATE_AMOUNT = '0.000010'
    process.env.LABWEAVER_E2E_WORK_GPU_RATE_CURRENCY = 'USD'
    process.env.LABWEAVER_E2E_WORK_GPU_RATE_EFFECTIVE_UNTIL = '2026-10-04T07:00:00Z'

    expect(realWorkGpuConfig()).toEqual({
      class: 'nvidia-cuda',
      mode: 'container_time_slice',
      count: 1,
      rate: {
        unit: 'gpu_unit_second',
        unitQuantity: 1,
        amount: '0.000010',
        currency: 'USD',
        gpuClass: 'nvidia-cuda',
        gpuMode: 'container_time_slice',
        effectiveUntil: '2026-10-04T07:00:00Z',
      },
    })
  })

  it('rejects an end time without a complete GPU trial configuration', () => {
    process.env.LABWEAVER_E2E_WORK_GPU_RATE_EFFECTIVE_UNTIL = '2026-10-04T07:00:00Z'
    expect(() => realWorkGpuConfig()).toThrow('LABWEAVER_E2E_WORK_GPU_EFFECTIVE_UNTIL_WITHOUT_RATE')
  })
})

const VM_ENVIRONMENT = Object.freeze({
  LABWEAVER_E2E_VM_PROVIDER_BINDING: 'kubevirt-primary-v1',
  LABWEAVER_E2E_VM_STORAGE_CLASS_BINDING: 'vm-rwo-primary-v1',
  LABWEAVER_E2E_VM_BASE_DISK_BINDING: 'ubuntu-24.04-vgpu-v1',
  LABWEAVER_E2E_VM_BASE_DISK_SOURCE_REGISTRY_DIGEST: 'docker://quay.io/containerdisks/ubuntu@sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5',
  LABWEAVER_E2E_VM_BASE_DISK_CAPACITY_BYTES: String(10 * GIB),
  LABWEAVER_E2E_VM_SSH_PORT: '22',
})

describe('published Work resume retention', () => {
  const now = Date.parse('2026-10-03T10:15:00.000Z')
  const recovery = 'Generate, review, and publish a new Work template through the normal UI before retrying.'

  afterEach(() => vi.restoreAllMocks())

  function resumedWork(retention) {
    const artifact = { id: 'artifact', kind: 'container', repository: 'harbor.lab.lan/work', digest: `sha256:${'a'.repeat(64)}` }
    const approval = { id: 'approval', candidateId: 'candidate', candidateRevision: 1, policyRevision: 1, trustRevision: 1, decision: 'approved' }
    const release = { id: 'release', projectId: 'project', agentRunId: 'run', candidateId: 'candidate', candidateRevision: 1, version: 1, runtimeKind: 'container', approval, artifact }
    const resume = { projectId: 'project', runId: 'run', releaseId: 'release', seedMarker: 'seed', persistenceMarker: 'persistence' }
    const responses = {
      '/api/v1/auth/session': { actor: { actorId: 'actor' } },
      '/api/v1/projects/project': { id: 'project', ownerActorId: 'actor' },
      '/api/v1/projects/project/agent-runs/run': {
        id: 'run', projectId: 'project', state: 'succeeded', purpose: { kind: 'authoring', environmentClass: 'work' },
        tracks: [{ kind: 'environment', candidateId: 'candidate' }], packageId: 'package',
      },
      '/api/v1/projects/project/environment-candidates/candidate': {
        candidate: { id: 'candidate', runId: 'run', projectId: 'project', revision: 1, policyRevision: 1, spec: { class: 'work', runtime: { kind: 'container' }, retention } },
        trustRevision: 1, approvals: [approval], imageArtifact: artifact, build: { state: 'succeeded', artifact },
      },
      '/api/v1/projects/project/environment-template-releases/release': release,
      '/api/v1/projects/project/problem-packages/package': { id: 'package', projectId: 'project', revision: 1 },
    }
    const request = { get: vi.fn(async (url) => ({ ok: () => true, text: async () => JSON.stringify(responses[url]) })) }
    vi.spyOn(Date, 'now').mockReturnValue(now)
    return { request, resume, release }
  }

  it('continues a bound release with future retention', async () => {
    const { request, resume, release } = resumedWork({ retainUntil: '2026-10-04T10:15:00.123456Z' })
    await expect(readResumablePublishedWork(request, resume)).resolves.toMatchObject({ release })
  })

  it.each(['2026-10-03T10:15:00.000Z', '2026-10-01T14:32:04.364Z'])('rejects retention at or before now: %s', async (retainUntil) => {
    const { request, resume } = resumedWork({ retainUntil })
    await expect(readResumablePublishedWork(request, resume)).rejects.toThrow(`REAL_WORK_RESUME_RETENTION_EXPIRED: ${recovery}`)
    expect(request.get).not.toHaveBeenCalledWith('/api/v1/projects/project/problem-packages/package')
  })

  it.each([undefined, {}, { retainUntil: null }, { retainUntil: 123 }, { retainUntil: '' }, { retainUntil: 'not a timestamp' }])('rejects missing or malformed retention: %j', async (retention) => {
    const { request, resume } = resumedWork(retention)
    await expect(readResumablePublishedWork(request, resume)).rejects.toThrow(`REAL_WORK_RESUME_RETENTION_INVALID: ${recovery}`)
    expect(request.get).not.toHaveBeenCalledWith('/api/v1/projects/project/problem-packages/package')
  })
})
const VM_ENVIRONMENT_KEYS = Object.keys(VM_ENVIRONMENT)
const GPU_ENVIRONMENT_KEYS = [
  'LABWEAVER_E2E_WORK_GPU_CLASS',
  'LABWEAVER_E2E_WORK_GPU_MODE',
  'LABWEAVER_E2E_WORK_GPU_RATE_AMOUNT',
  'LABWEAVER_E2E_WORK_GPU_RATE_CURRENCY',
  'LABWEAVER_E2E_WORK_GPU_RATE_EFFECTIVE_UNTIL',
]
const savedEnvironment = new Map([...VM_ENVIRONMENT_KEYS, ...GPU_ENVIRONMENT_KEYS].map((key) => [key, process.env[key]]))
const savedProviderOptIn = process.env.LABWEAVER_E2E_REAL_PROVIDER

function setVmEnvironment(overrides = {}) {
  for (const key of VM_ENVIRONMENT_KEYS) process.env[key] = overrides[key] ?? VM_ENVIRONMENT[key]
}

afterEach(() => {
  for (const [key, value] of savedEnvironment) {
    if (value === undefined) delete process.env[key]
    else process.env[key] = value
  }
  if (savedProviderOptIn === undefined) delete process.env.LABWEAVER_E2E_REAL_PROVIDER
  else process.env.LABWEAVER_E2E_REAL_PROVIDER = savedProviderOptIn
})

describe('Real finance adjustment helper', () => {
  const charge = {
    id: 'charge-original',
    projectId: 'project',
    settlement: 'settled',
    adjustmentOf: null,
    total: { currency: 'USD', amount: '0.000001' },
  }

  it('rejects a derived adjustment charge before opening a browser context', async () => {
    const browser = { newContext: vi.fn() }
    await expect(verifyRealWorkFinanceAdjustmentByUi(browser, 'http://localhost:8080', 'project', {
      ...charge,
      adjustmentOf: 'charge-source',
    })).rejects.toThrow('REAL_WORK_FINANCE_ADJUSTMENT_CHARGE_NOT_ORIGINAL')
    expect(browser.newContext).not.toHaveBeenCalled()
  })

  it('requires at least one micro-dollar in the original settled charge', async () => {
    const browser = { newContext: vi.fn() }
    await expect(verifyRealWorkFinanceAdjustmentByUi(browser, 'http://localhost:8080', 'project', {
      ...charge,
      total: { currency: 'USD', amount: '0.000000' },
    })).rejects.toThrow('REAL_WORK_FINANCE_ADJUSTMENT_CHARGE_TOO_SMALL')
    expect(browser.newContext).not.toHaveBeenCalled()
  })
})

describe('real Work VM package', () => {
  it('uses exact reviewed VM catalog values and does not require a container image', () => {
    setVmEnvironment()
    process.env.LABWEAVER_E2E_REAL_PROVIDER = '1'

    expect(realWorkConfig({ virtualMachine: true })).toBeNull()
    expect(realWorkVmConfig()).toEqual({
      providerBinding: 'kubevirt-primary-v1',
      storageClassBinding: 'vm-rwo-primary-v1',
      baseDisk: {
        binding: 'ubuntu-24.04-vgpu-v1',
        sourceRegistryDigest: VM_ENVIRONMENT.LABWEAVER_E2E_VM_BASE_DISK_SOURCE_REGISTRY_DIGEST,
        capacityBytes: 10 * GIB,
      },
      sshPort: 22,
    })
  })

  it('rejects unsafe imported disk capacity values', () => {
    setVmEnvironment({ LABWEAVER_E2E_VM_BASE_DISK_CAPACITY_BYTES: '9007199254740992' })
    expect(() => realWorkVmConfig()).toThrow('LABWEAVER_E2E_VM_BASE_DISK_CAPACITY_INVALID')
  })

  it('builds Work materials with the reviewed VM runtime and mutable authorized guest workspace', async () => {
    setVmEnvironment()
    const vm = realWorkVmConfig()
    const gpu = { class: 'nvidia-v100-2q', count: 1 }
    const packageCopy = await createRealWorkPackage(null, { vm, gpu })
    try {
      const serialized = JSON.parse(await readFile(`${packageCopy.directory}/environment-spec.json`, 'utf8'))
      const readme = await readFile(`${packageCopy.directory}/README.md`, 'utf8')
      const configuration = await readFile(`${packageCopy.directory}/work-configuration.md`, 'utf8')
      const packageFiles = (await readdir(packageCopy.directory)).sort()

      expect(packageCopy.seedMarker).toBeNull()
      expect(packageCopy.dockerfile).toBeNull()
      expect(packageCopy.environmentSpec).toEqual(serialized.environmentSpec)
      expect(packageCopy.environmentSpec).toMatchObject({
        class: 'work',
        resources: {
          cpuMillicores: 1000,
          memoryBytes: 2 * GIB,
          storageBytes: 16 * GIB,
          gpu,
        },
        network: { mode: 'deny_all' },
        entries: [{ name: 'ssh', protocol: 'ssh', servicePort: 22 }],
        security: {
          userPolicy: 'non_root_required',
          rootFilesystemPolicy: 'mutable_required',
          privilegeEscalationPolicy: 'deny',
          publicExposurePolicy: 'deny',
        },
        runtime: {
          kind: 'virtual_machine',
          provider_binding: vm.providerBinding,
          base_disk: vm.baseDisk,
          storage_class_binding: vm.storageClassBinding,
          ssh_port: 22,
        },
      })
      expect(packageCopy.environmentSpec.runtime).not.toHaveProperty('build_recipe')
      expect(readme).toContain(vm.baseDisk.sourceRegistryDigest)
      expect(readme).toContain('Do not replace the VM with a container')
      expect(configuration).toContain('persistence-marker.txt in the current directory')
      expect(configuration).toContain('Do not use sudo or alter the SSH access policy')
      expect(configuration).not.toContain('/workspace/')
      expect(packageFiles).toEqual(['README.md', 'environment-spec.json', 'work-configuration.md'])
    } finally {
      await packageCopy.cleanup()
    }
  })

  it('accepts only the Work candidate and Control-resolved VM artifact matching that catalog entry', async () => {
    setVmEnvironment()
    const vm = realWorkVmConfig()
    const packageCopy = await createRealWorkPackage(null, { vm, gpu: { class: 'nvidia-v100-2q', count: 1 } })
    try {
      const validCandidate = {
        candidate: { spec: packageCopy.environmentSpec },
        build: null,
        imageArtifact: {
          id: '01900000-0000-7000-8000-000000000002',
          kind: 'virtual_machine',
          base_disk: vm.baseDisk,
          format: 'qcow2',
        },
      }
      expect(assertRealWorkVmCandidate(validCandidate, vm, { class: 'nvidia-v100-2q', count: 1 }))
        .toEqual(validCandidate.imageArtifact)
      expect(() => assertRealWorkVmCandidate({
        ...validCandidate,
        imageArtifact: { ...validCandidate.imageArtifact, base_disk: { ...vm.baseDisk, binding: 'other-base' } },
      }, vm, { class: 'nvidia-v100-2q', count: 1 })).toThrow('REAL_WORK_VM_CANDIDATE_ARTIFACT_INVALID')
      expect(() => assertRealWorkVmCandidate({
        ...validCandidate,
        candidate: {
          spec: {
            ...packageCopy.environmentSpec,
            runtime: { ...packageCopy.environmentSpec.runtime, build_recipe: { mode: 'generated' } },
          },
        },
      }, vm, { class: 'nvidia-v100-2q', count: 1 })).toThrow('REAL_WORK_VM_CANDIDATE_SPEC_INVALID')
      expect(() => assertRealWorkVmCandidate({ ...validCandidate, build: { state: 'succeeded' } }, vm))
        .toThrow('REAL_WORK_VM_CANDIDATE_SPEC_INVALID')
    } finally {
      await packageCopy.cleanup()
    }
  })
})


describe('GPU container Work material and candidate', () => {
  const image = `harbor.example.test/python@sha256:${'a'.repeat(64)}`
  const gpu = { class: 'nvidia-cuda', count: 1 }
  const providerBinding = 'container-primary-v1'

  it('preserves the HTTP seed recipe alongside the owner CUDA terminal', async () => {
    const material = await createRealWorkPackage(image, { gpu, providerBinding })
    try {
      expect(material.environmentSpec).toMatchObject({
        class: 'work', resources: { gpu },
        runtime: {
          kind: 'container', provider_binding: providerBinding, service_port: 8080,
          terminal: { executable: '/bin/sh', args: [], workingDirectory: '/workspace' },
        },
      })
      expect(material.dockerfile).toContain(`FROM ${image}`)
      expect(material.dockerfile).toContain('COPY seed.txt /opt/labweaver/workspace-seed/seed.txt')
      expect(material.dockerfile).toContain('"http.server", "8080"')
      const readme = await readFile(`${material.directory}/README.md`, 'utf8')
      expect(readme).toContain('class=work')
      expect(readme).toContain('count 1')
      expect(readme).toContain('256 threads, sum 32640, max 255')
      expect(readme).toContain('libcuda.so.1 must be supplied by the normal NVIDIA runtime')
      expect(() => assertRealWorkGpuContainerCandidate({ candidate: { spec: material.environmentSpec } }, providerBinding, gpu)).not.toThrow()
    } finally {
      await material.cleanup()
    }
  })

  it('rejects changed class, GPU allocation, provider, or terminal before approval', async () => {
    const material = await createRealWorkPackage(image, { gpu, providerBinding })
    try {
      const invalidChanges = [
        (spec) => { spec.class = 'experiment' },
        (spec) => { spec.resources.gpu.class = 'other-gpu' },
        (spec) => { spec.resources.gpu.count = 2 },
        (spec) => { spec.runtime.provider_binding = 'other-provider' },
        (spec) => { delete spec.runtime.terminal },
        (spec) => { spec.runtime.terminal.executable = '/bin/bash' },
        (spec) => { spec.runtime.terminal.args = ['-c', 'true'] },
        (spec) => { spec.runtime.terminal.workingDirectory = '/tmp' },
      ]
      for (const change of invalidChanges) {
        const spec = structuredClone(material.environmentSpec)
        change(spec)
        expect(() => assertRealWorkGpuContainerCandidate({ candidate: { spec } }, providerBinding, gpu))
          .toThrow('REAL_WORK_GPU_CONTAINER_CANDIDATE_SPEC_INVALID')
      }
    } finally {
      await material.cleanup()
    }
  })

  it('keeps CPU container Work without a GPU terminal requirement', async () => {
    const material = await createRealWorkPackage(image, { providerBinding })
    try {
      expect(material.environmentSpec.runtime).not.toHaveProperty('terminal')
      expect(material.environmentSpec.resources).not.toHaveProperty('gpu')
      expect(material.dockerfile).toContain('"http.server", "8080"')
    } finally {
      await material.cleanup()
    }
  })
})

describe('real Work cleanup through visible owner controls', () => {
  const baseURL = 'https://portal.example.test'
  const environmentPath = '/api/v1/environments/environment'
  const leasePath = '/api/v1/resource-leases/lease'
  const requestPath = '/api/v1/resource-requests/request'
  const ready = {
    id: 'environment', projectId: 'project', leaseId: 'lease', revision: 4,
    desiredState: 'running', observedState: 'ready',
  }
  const stopped = { ...ready, revision: 5, desiredState: 'stopped', observedState: 'stopped' }
  const failed = {
    ...stopped, desiredState: 'deleted', observedState: 'failed',
    operation: { id: 'expire', kind: 'expire', state: 'failed', acceptedRevision: 3 },
  }
  const deleted = { ...failed, revision: 6, observedState: 'deleted', operation: { id: 'delete', kind: 'delete', state: 'succeeded', acceptedRevision: 5 } }
  const activeLease = { id: 'lease', requestId: 'request', claimId: 'claim', revision: 3, state: 'active' }
  const revokedLease = { ...activeLease, state: 'revoked' }
  const expiredRequest = { id: 'request', projectId: 'project', state: 'expired' }

  function response(body, status = 200) {
    return { ok: () => status >= 200 && status < 300, status: () => status, text: async () => JSON.stringify(body) }
  }

  function http(sequences) {
    const snapshots = new Map(Object.entries(sequences).map(([path, values]) => [path, [...values]]))
    function next(path) {
      const values = snapshots.get(path)
      if (!values?.length) throw new Error(`UNEXPECTED_READ:${path}`)
      return values.length > 1 ? values.shift() : values[0]
    }
    return { get: vi.fn(async (path) => response(next(path))) }
  }

  beforeEach(() => {
    vi.clearAllMocks()
    cleanupUi.stopEnvironment.mockResolvedValue(stopped)
    cleanupUi.releaseLease.mockResolvedValue({ ...revokedLease, released: true })
    cleanupUi.deleteEnvironment.mockResolvedValue(deleted)
    cleanupUi.cancelRequest.mockResolvedValue({ ...expiredRequest, state: 'cancelled' })
  })

  afterEach(() => vi.restoreAllMocks())

  it('uses the visible stop, lease release, and delete controls in order', async () => {
    const request = http({
      [environmentPath]: [ready, stopped, deleted],
      [leasePath]: [activeLease, revokedLease, revokedLease],
      [requestPath]: [expiredRequest],
    })
    await cleanupWorkResources(request, baseURL, 'project', 'environment', 'lease', 'request', { kind: 'page' })
    expect(cleanupUi.stopEnvironment).toHaveBeenCalledWith(expect.anything(), expect.objectContaining({ projectId: 'project', environmentId: 'environment' }))
    expect(cleanupUi.releaseLease).toHaveBeenCalledWith(expect.anything(), { projectId: 'project', requestId: 'request', leaseId: 'lease' })
    expect(cleanupUi.deleteEnvironment).toHaveBeenCalledWith(expect.anything(), expect.objectContaining({ projectId: 'project', environmentId: 'environment' }))
    expect(cleanupUi.stopEnvironment.mock.invocationCallOrder[0]).toBeLessThan(cleanupUi.releaseLease.mock.invocationCallOrder[0])
    expect(cleanupUi.releaseLease.mock.invocationCallOrder[0]).toBeLessThan(cleanupUi.deleteEnvironment.mock.invocationCallOrder[0])
    expect(request.get).not.toHaveBeenCalledWith(expect.stringContaining('/cancel'))
  })

  it('cancels a pending resource request through the visible resource page', async () => {
    const request = http({
      '/api/v1/projects/project/resource-leases': [[]],
      [requestPath]: [{ id: 'request', projectId: 'project', requestKey: 'run:resource', state: 'reviewing' }, { ...expiredRequest, state: 'cancelled' }],
      [environmentPath]: [deleted],
    })
    await cleanupWorkResources(request, baseURL, 'project', 'environment', null, 'request', { kind: 'page' })
    expect(cleanupUi.cancelRequest).toHaveBeenCalledWith(expect.anything(), { projectId: 'project', requestKey: 'run:resource' })
    expect(cleanupUi.stopEnvironment).not.toHaveBeenCalled()
    expect(cleanupUi.deleteEnvironment).not.toHaveBeenCalled()
  })

  it('re-reads an active request lease before releasing it', async () => {
    const request = http({
      '/api/v1/projects/project/resource-leases': [[], [activeLease]],
      [requestPath]: [
        { id: 'request', projectId: 'project', requestKey: 'run:resource', state: 'active' },
        { ...expiredRequest, requestKey: 'run:resource' },
      ],
      [environmentPath]: [ready, stopped, deleted],
      [leasePath]: [activeLease, revokedLease, revokedLease],
    })
    await cleanupWorkResources(request, baseURL, 'project', 'environment', null, 'request', { kind: 'page' })
    expect(cleanupUi.releaseLease).toHaveBeenCalledWith(expect.anything(), { projectId: 'project', requestId: 'request', leaseId: 'lease' })
    expect(cleanupUi.cancelRequest).not.toHaveBeenCalled()
  })

  it('requires a browser page before attempting cleanup mutations', async () => {
    await expect(cleanupWorkResources({ get: vi.fn() }, baseURL, 'project', 'environment', null, null))
      .rejects.toThrow('REAL_WORK_CLEANUP_UI_PAGE_REQUIRED')
  })
})
