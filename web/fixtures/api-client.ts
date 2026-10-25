import { activeScene, fixtureProjectId } from './scenes'
import type {
  ProblemDetails,
  ResourceBudgetSchema,
} from '../src/generated/contracts/types.gen'

type FixtureSuccess<T> = { data: T; error: undefined }
type FixtureError = { data: undefined; error: unknown }
type FixtureResult<T> = FixtureSuccess<T> | FixtureError
type FixtureData = ResourceBudgetSchema
type FixtureDataResult = { kind: 'data'; data: FixtureData } | { kind: 'error'; error: unknown }

const now = '2026-09-14T08:00:00.000Z'

const budget = {
  id: 'budget-physics-fixture',
  projectId: fixtureProjectId,
  limit: { currency: 'USD', amount: '125000.000000' },
  warningAt: { currency: 'USD', amount: '100000.000000' },
  spent: { currency: 'USD', amount: '84732.450000' },
  revision: 9,
  updatedAt: now,
} satisfies ResourceBudgetSchema

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
  const fixtureBudgetUrl = `/api/v1/projects/${encodeURIComponent(fixtureProjectId)}/resource-budget`
  if (url === fixtureBudgetUrl) {
    return activeScene.value.id === 'budget-empty'
      ? { kind: 'error', error: 'LW_RESOURCE_BUDGET_NOT_FOUND' }
      : { kind: 'data', data: budget }
  }
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
