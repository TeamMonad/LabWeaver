import { beforeEach, describe, expect, it, vi } from 'vitest'
import { ref } from 'vue'
import { useConsoleCapability } from '@/composables/useConsoleCapability'
import { issueConsoleCapability, listConsoleCapabilities } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    issueConsoleCapability: vi.fn(),
    listConsoleCapabilities: vi.fn(),
  }
})

function makeAvailability(overrides: Record<string, unknown> = {}) {
  return {
    accessGrantId: 'grant-1',
    accessGrantRevision: 3,
    environmentClass: 'experiment',
    environmentId: 'env-1',
    environmentRevision: 5,
    expiresAt: '2026-07-16T09:00:00.000Z',
    kinds: ['xterm'],
    leaseFence: null,
    ...overrides,
  }
}

function makeCapability() {
  return {
    id: 'cap-1',
    accessGrantId: 'grant-1',
    accessGrantRevision: 3,
    environmentClass: 'experiment',
    environmentId: 'env-1',
    environmentRevision: 5,
    kind: 'xterm',
    connectionLocator: '/api/v1/console-sessions/session-1',
    websocketSubprotocol: 'labweaver.console.xterm.v1',
    issuedAt: '2026-07-16T08:00:00.000Z',
    expiresAt: '2026-07-16T09:00:00.000Z',
    leaseFence: null,
  }
}

describe('useConsoleCapability', () => {
  beforeEach(() => {
    vi.resetAllMocks()
  })

  it('loads availability', async () => {
    vi.mocked(listConsoleCapabilities).mockResolvedValue({ data: makeAvailability(), error: undefined as never })
    const grantId = ref('grant-1')
    const capability = useConsoleCapability(grantId)
    await capability.load()
    expect(capability.availability.kind).toBe('success')
    if (capability.availability.kind === 'success') {
      expect(capability.availability.data.kinds).toEqual(['xterm'])
    }
  })

  it('surfaces list error as diagnostic', async () => {
    vi.mocked(listConsoleCapabilities).mockResolvedValue({
      data: undefined as never,
      error: { response: { data: { diagnosticCode: 'ACCESS_GRANT_NOT_ACTIVE', detail: 'revoked', retryable: false } } },
    })
    const grantId = ref('grant-1')
    const capability = useConsoleCapability(grantId)
    await capability.load()
    expect(capability.availability.kind).toBe('error')
    if (capability.availability.kind === 'error') {
      expect(capability.availability.diagnostic.code).toBe('ACCESS_GRANT_NOT_ACTIVE')
    }
  })

  it('issues a capability with expected revisions', async () => {
    vi.mocked(listConsoleCapabilities).mockResolvedValue({ data: makeAvailability(), error: undefined as never })
    vi.mocked(issueConsoleCapability).mockResolvedValue({ data: makeCapability(), error: undefined as never })
    const grantId = ref('grant-1')
    const capability = useConsoleCapability(grantId)
    await capability.load()
    const result = await capability.issue('xterm', { environmentRevision: 5, leaseFence: null })
    expect(result.ok).toBe(true)
    expect(issueConsoleCapability).toHaveBeenCalledWith(
      expect.objectContaining({
        body: expect.objectContaining({
          kind: 'xterm',
          expectedAccessGrantRevision: 3,
          expectedEnvironmentRevision: 5,
        }),
      }),
    )
  })

  it('surfaces issue error as diagnostic', async () => {
    vi.mocked(listConsoleCapabilities).mockResolvedValue({ data: makeAvailability(), error: undefined as never })
    vi.mocked(issueConsoleCapability).mockResolvedValue({
      data: undefined as never,
      error: { response: { data: { diagnosticCode: 'REVISION_CONFLICT', detail: 'stale', retryable: false } } },
    })
    const grantId = ref('grant-1')
    const capability = useConsoleCapability(grantId)
    await capability.load()
    const result = await capability.issue('xterm', { environmentRevision: 5, leaseFence: null })
    expect(result.ok).toBe(false)
    expect(result.diagnostic?.code).toBe('REVISION_CONFLICT')
  })

  it('rejects issue when availability not ready', async () => {
    const grantId = ref('grant-1')
    const capability = useConsoleCapability(grantId)
    const result = await capability.issue('xterm', { environmentRevision: 5, leaseFence: null })
    expect(result.ok).toBe(false)
    expect(result.diagnostic?.code).toBe('CONSOLE_CAPABILITY_NOT_READY')
  })
})
