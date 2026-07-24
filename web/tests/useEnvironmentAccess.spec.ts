import { afterEach, describe, expect, it, vi } from 'vitest'
import { ref } from 'vue'
import { useEnvironmentAccess } from '@/composables/useEnvironmentAccess'
import { createAccessGrant, getAccessGrant, listEnvironmentEndpoints } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    createAccessGrant: vi.fn(),
    getAccessGrant: vi.fn(),
    listEnvironmentEndpoints: vi.fn(),
    revokeAccessGrant: vi.fn(),
  }
})

function grant(state: 'requested' | 'active') {
  return {
    id: '01900000-0000-7000-8000-000000000010',
    actorId: '01900000-0000-7000-8000-000000000011',
    courseId: '01900000-0000-7000-8000-000000000012',
    environmentId: '01900000-0000-7000-8000-000000000013',
    environmentRevision: 1,
    state,
    revision: state === 'requested' ? 1 : 2,
    endpointGrants:
      state === 'active'
        ? [
            {
              id: '01900000-0000-7000-8000-000000000014',
              accessGrantId: '01900000-0000-7000-8000-000000000010',
              endpointId: '01900000-0000-7000-8000-000000000015',
              endpointRevision: 1,
              protocol: 'https' as const,
              action: 'connect' as const,
              health: 'healthy' as const,
              alias: null,
              connectUrl: '/connect/01900000-0000-7000-8000-000000000014/',
              expiresAt: '2026-07-19T10:00:00.000Z',
            },
          ]
        : [],
    issuedAt: '2026-07-19T09:00:00.000Z',
    expiresAt: '2026-07-19T10:00:00.000Z',
    revokedAt: null,
    reasonCode: null,
  }
}

afterEach(() => {
  vi.useRealTimers()
  vi.clearAllMocks()
})

describe('useEnvironmentAccess', () => {
  it('fails closed when endpoint discovery returns no data or typed error', async () => {
    vi.mocked(listEnvironmentEndpoints).mockResolvedValue({} as never)
    const access = useEnvironmentAccess(
      ref('01900000-0000-7000-8000-000000000013'),
      ref(1),
      ref('01900000-0000-7000-8000-000000000012'),
    )

    await access.loadEndpoints()

    expect(access.endpoints.kind).toBe('error')
    if (access.endpoints.kind === 'error') {
      expect(access.endpoints.diagnostic.code).toBe('ENDPOINT_LIST_RESPONSE_INVALID')
      expect(access.endpoints.diagnostic.retryable).toBe(false)
    }
  })

  it('waits for the authoritative active grant before exposing a connect URL', async () => {
    vi.useFakeTimers()
    vi.mocked(listEnvironmentEndpoints).mockResolvedValue({
      data: {
        items: [
          {
            id: '01900000-0000-7000-8000-000000000015',
            protocol: 'https',
            revision: 1,
            health: 'healthy',
            observedAt: '2026-07-19T09:00:00.000Z',
          },
        ],
      },
    } as never)
    vi.mocked(createAccessGrant).mockResolvedValue({ data: grant('requested') } as never)
    vi.mocked(getAccessGrant).mockResolvedValue({ data: grant('active') } as never)
    const access = useEnvironmentAccess(
      ref('01900000-0000-7000-8000-000000000013'),
      ref(1),
      ref('01900000-0000-7000-8000-000000000012'),
    )

    await access.loadEndpoints()
    const pending = access.createGrant()
    await vi.advanceTimersByTimeAsync(500)
    await expect(pending).resolves.toEqual({ ok: true })
    expect(access.grant.kind).toBe('success')
    if (access.grant.kind === 'success') {
      expect(access.grant.data.state).toBe('active')
      expect(access.grant.data.endpointGrants[0]?.connectUrl).toBe(
        '/connect/01900000-0000-7000-8000-000000000014/',
      )
    }
  })
})
