import { activeScene, fixtureProjectId } from './scenes'
import type {
  GpuCatalogEntrySchema,
  ProblemDetails,
  ResourceBudgetSchema,
  ResourceChargeSchema,
  ResourceRateSchema,
} from '../src/generated/contracts/types.gen'

type FixtureSuccess<T> = { data: T; error: undefined }
type FixtureError = { data: undefined; error: unknown }
type FixtureResult<T> = FixtureSuccess<T> | FixtureError
type FixtureData =
  | GpuCatalogEntrySchema[]
  | ResourceRateSchema[]
  | ResourceBudgetSchema
  | ResourceChargeSchema[]
type FixtureDataResult = { kind: 'data'; data: FixtureData } | { kind: 'error'; error: unknown }

const now = '2026-09-14T08:00:00.000Z'

const catalog = [
  {
    id: 'gpu-a10-fixture',
    class: 'nvidia-a10',
    mode: 'exclusive',
    capacityUnits: 4,
    revision: 2,
    active: true,
    providerBinding: 'fixture-kubernetes',
    allocationBinding: 'fixture-nvidia-a10',
  },
] satisfies GpuCatalogEntrySchema[]

const rates = [
  { id: 'rate-cpu-fixture', revision: 1, unit: 'cpu_millicore_second', unitQuantity: 1000, gpuClass: null, gpuMode: null, unitPrice: { currency: 'USD', amount: '0.000010' }, effectiveFrom: '2026-01-01T00:00:00.000Z', effectiveUntil: null },
  { id: 'rate-gpu-fixture', revision: 1, unit: 'gpu_unit_second', unitQuantity: 1, gpuClass: 'nvidia-a10', gpuMode: 'exclusive', unitPrice: { currency: 'USD', amount: '0.000800' }, effectiveFrom: '2026-01-01T00:00:00.000Z', effectiveUntil: null },
] satisfies ResourceRateSchema[]

const budget = {
  id: 'budget-physics-fixture',
  projectId: fixtureProjectId,
  limit: { currency: 'USD', amount: '125000.000000' },
  warningAt: { currency: 'USD', amount: '100000.000000' },
  spent: { currency: 'USD', amount: '84732.450000' },
  revision: 9,
  updatedAt: now,
} satisfies ResourceBudgetSchema

const charges = [{
  id: 'charge-physics-september',
  usageRecordId: 'usage-physics-september',
  projectId: fixtureProjectId,
  lines: [
    { rateId: 'rate-cpu-fixture', rateRevision: 1, unit: 'cpu_millicore_second', quantity: 720000000000, unitQuantity: 1000, unitPrice: { currency: 'USD', amount: '0.000010' }, amount: { currency: 'USD', amount: '7200.000000' } },
    { rateId: 'rate-gpu-fixture', rateRevision: 1, unit: 'gpu_unit_second', quantity: 216000, unitQuantity: 1, unitPrice: { currency: 'USD', amount: '0.000800' }, amount: { currency: 'USD', amount: '172.800000' } },
  ],
  total: { currency: 'USD', amount: '7372.800000' },
  settlement: 'pending',
  createdAt: now,
}] satisfies ResourceChargeSchema[]

function unsupported(operation: string): ProblemDetails {
  return {
    type: 'about:blank',
    title: '预览操作未覆盖',
    status: 501,
    detail: 'Fixture 预览未覆盖 ' + operation + '，不能执行真实写操作。',
    instance: '/fixture-preview',
    requestId: 'fixture-operation-unsupported',
    diagnosticCode: 'FIXTURE_OPERATION_UNSUPPORTED',
    retryable: false,
  }
}

function getData(url: string): FixtureDataResult {
  if (url.endsWith('/resource/gpu-catalog')) return { kind: 'data', data: catalog }
  if (url.endsWith('/resource/rates')) return { kind: 'data', data: rates }
  const fixtureBudgetUrl = `/api/v1/projects/${encodeURIComponent(fixtureProjectId)}/resource-budget`
  if (url === fixtureBudgetUrl) {
    return activeScene.value.id === 'budget-empty'
      ? { kind: 'error', error: 'LW_RESOURCE_BUDGET_NOT_FOUND' }
      : { kind: 'data', data: budget }
  }
  const fixtureChargesUrl = `/api/v1/projects/${encodeURIComponent(fixtureProjectId)}/charges`
  if (url === fixtureChargesUrl) return { kind: 'data', data: activeScene.value.id === 'budget-large' ? charges : [] }
  return { kind: 'error', error: unsupported(`GET ${url}`) }
}

export const apiClient = {
  async get(options: { url: string }): Promise<FixtureResult<unknown>> {
    const result = getData(options.url)
    return result.kind === 'error' ? { data: undefined, error: result.error } : { data: result.data, error: undefined }
  },
  async post(options: { url?: string } = {}): Promise<FixtureResult<unknown>> {
    return { data: undefined, error: unsupported(`POST ${options.url ?? '未知端点'}`) }
  },
  async put(options: { url?: string } = {}): Promise<FixtureResult<unknown>> {
    return { data: undefined, error: unsupported(`PUT ${options.url ?? '未知端点'}`) }
  },
}
