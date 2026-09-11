import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { effectScope, ref } from 'vue'
import { useEnvironmentOperations } from '@/composables/useEnvironmentOperations'
import { cancelEnvironmentOperation, listEnvironmentOperations } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    cancelEnvironmentOperation: vi.fn(),
    listEnvironmentOperations: vi.fn(),
  }
})

function snapshot(overrides: Record<string, unknown> = {}) {
  return {
    environmentId: 'env-1',
    operationId: 'op-1',
    kind: 'start',
    state: 'running',
    acceptedRevision: 3,
    acceptedAt: '2026-07-11T10:00:00.000Z',
    deadlineAt: '2026-07-11T10:05:00.000Z',
    attempt: 1,
    maxAttempts: 3,
    retryEligible: false,
    cancelEligible: true,
    diagnosticCode: null,
    traceId: 'trace-1',
    terminalAt: null,
    cleanupStartedAt: null,
    ...overrides,
  }
}

function withScope(environmentId: ReturnType<typeof ref<string | undefined>>) {
  const scope = effectScope()
  const operations = scope.run(() => useEnvironmentOperations(environmentId))
  if (!operations) throw new Error('failed to create operations composable')
  return { operations, stop: () => scope.stop() }
}

describe('useEnvironmentOperations', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    vi.mocked(listEnvironmentOperations).mockResolvedValue({ data: { items: [] }, error: undefined as never })
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('loads the public operation snapshots into a successful state', async () => {
    const item = snapshot()
    vi.mocked(listEnvironmentOperations).mockResolvedValue({
      data: { items: [item] },
      error: undefined as never,
    } as never)
    const environmentId = ref<string | undefined>('env-1')
    const { operations, stop } = withScope(environmentId)

    await vi.waitFor(() => expect(operations.operations).toEqual({ kind: 'success', data: [item] }))

    stop()
  })

  it('surfaces list failures instead of treating an unavailable history as empty', async () => {
    vi.mocked(listEnvironmentOperations).mockResolvedValue({
      data: undefined,
      error: {
        diagnosticCode: 'OPERATION_LIST_UNAVAILABLE',
        detail: '操作历史暂时不可用',
        retryable: false,
      },
    } as never)
    const environmentId = ref<string | undefined>('env-1')
    const { operations, stop } = withScope(environmentId)

    await vi.waitFor(() => expect(operations.operations.kind).toBe('error'))
    expect(operations.operations).toEqual({
      kind: 'error',
      diagnostic: {
        code: 'OPERATION_LIST_UNAVAILABLE',
        message: '操作历史暂时不可用',
        retryable: false,
      },
    })

    stop()
  })

  it('ignores an old response after the selected environment changes', async () => {
    let resolveFirst: (value: unknown) => void = () => undefined
    const firstResponse = new Promise<unknown>((resolve) => {
      resolveFirst = resolve
    })
    const secondItem = snapshot({ environmentId: 'env-2', operationId: 'op-2' })
    vi.mocked(listEnvironmentOperations).mockImplementation(({ path }) => {
      if (path.environmentId === 'env-1') return firstResponse as never
      return Promise.resolve({ data: { items: [secondItem] }, error: undefined }) as never
    })
    const environmentId = ref<string | undefined>('env-1')
    const { operations, stop } = withScope(environmentId)

    await vi.waitFor(() => expect(listEnvironmentOperations).toHaveBeenCalledTimes(1))
    environmentId.value = 'env-2'
    await vi.waitFor(() => expect(listEnvironmentOperations).toHaveBeenCalledWith({ path: { environmentId: 'env-2' } }))
    await vi.waitFor(() => expect(operations.operations).toEqual({ kind: 'success', data: [secondItem] }))

    resolveFirst({ data: { items: [snapshot()] }, error: undefined })
    await Promise.resolve()
    expect(operations.operations).toEqual({ kind: 'success', data: [secondItem] })

    stop()
  })

  it('cancels with a fresh idempotency key and the current environment revision', async () => {
    const accepted = {
      environmentId: 'env-1',
      operationId: 'op-cancel',
      revision: 4,
      statusUrl: '/api/v1/operations/op-cancel',
    }
    vi.mocked(cancelEnvironmentOperation).mockResolvedValue({ data: accepted, error: undefined as never } as never)
    const environmentId = ref<string | undefined>('env-1')
    const { operations, stop } = withScope(environmentId)

    await expect(operations.cancel('env-1', 9)).resolves.toEqual({ ok: true, accepted })
    expect(cancelEnvironmentOperation).toHaveBeenCalledWith({
      path: { environmentId: 'env-1' },
      headers: {
        'Idempotency-Key': expect.any(String),
        'If-Match': '"rev-9"',
      },
    })
    expect(operations.cancelling).toBe(false)

    stop()
  })

  it('keeps cancellation diagnostics visible and returns a failed mutation result', async () => {
    vi.mocked(cancelEnvironmentOperation).mockResolvedValue({
      data: undefined,
      error: {
        diagnosticCode: 'OPERATION_CANCEL_CONFLICT',
        detail: '环境修订版本已变化',
        retryable: true,
      },
    } as never)
    const environmentId = ref<string | undefined>('env-1')
    const { operations, stop } = withScope(environmentId)

    await expect(operations.cancel('env-1', 9)).resolves.toEqual({
      ok: false,
      diagnostic: {
        code: 'OPERATION_CANCEL_CONFLICT',
        message: '环境修订版本已变化',
        retryable: true,
      },
    })
    expect(operations.cancelDiagnostic).toEqual({
      code: 'OPERATION_CANCEL_CONFLICT',
      message: '环境修订版本已变化',
      retryable: true,
    })
    expect(operations.cancelling).toBe(false)

    stop()
  })
})
