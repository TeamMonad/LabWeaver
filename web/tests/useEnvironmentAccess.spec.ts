import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ref } from 'vue'
import { useEnvironmentAccess } from '@/composables/useEnvironmentAccess'
import {
  createAccessGrant,
  getAccessGrant,
  listEnvironmentAccessGrants,
  listEnvironmentEndpoints,
  revokeAccessGrant,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    createAccessGrant: vi.fn(),
    getAccessGrant: vi.fn(),
    listEnvironmentAccessGrants: vi.fn(),
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
  it('restores exactly one authoritative active grant after a page reload', async () => {
    const active = grant('active')
    vi.mocked(listEnvironmentAccessGrants).mockResolvedValue({
      data: {
        items: [{ id: active.id }],
      },
    } as never)
    vi.mocked(getAccessGrant).mockResolvedValue({ data: active } as never)
    const access = useEnvironmentAccess(
      ref(active.environmentId),
      ref(active.environmentRevision),
      ref(active.courseId),
    )

    await expect(access.loadCurrentGrant()).resolves.toEqual({ ok: true })
    expect(access.grant).toEqual({ kind: 'success', data: active })
    expect(listEnvironmentAccessGrants).toHaveBeenCalledWith({
      path: { environmentId: active.environmentId },
      query: { state: 'active', includeTerminal: false, limit: 2 },
    })
  })

  it('fails closed when multiple active grants make console authority ambiguous', async () => {
    const active = grant('active')
    vi.mocked(listEnvironmentAccessGrants).mockResolvedValue({
      data: {
        items: [{ id: active.id }, { id: '01900000-0000-7000-8000-000000000099' }],
      },
    } as never)
    const access = useEnvironmentAccess(
      ref(active.environmentId),
      ref(active.environmentRevision),
      ref(active.courseId),
    )

    const result = await access.loadCurrentGrant()
    expect(result.ok).toBe(false)
    expect(access.grant.kind).toBe('error')
    if (access.grant.kind === 'error') {
      expect(access.grant.diagnostic.code).toBe('ACCESS_GRANT_ACTIVE_CONFLICT')
    }
    expect(getAccessGrant).not.toHaveBeenCalled()
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

  describe('out-of-order answers', () => {
    const environmentId = '01900000-0000-7000-8000-000000000013'

    beforeEach(() => {
      vi.resetAllMocks()
    })

    /** `Promise.withResolvers` is outside the project's ES2020 lib, so the executor form captures the resolver. */
    function deferred<T>() {
      let resolve!: (value: T) => void
      const promise = new Promise<T>((settle) => { resolve = settle })
      return { promise, resolve }
    }

    function endpoint(health: 'pending' | 'healthy') {
      return { id: '01900000-0000-7000-8000-000000000015', protocol: 'https', revision: 1, health, observedAt: '2026-07-19T09:00:00.000Z' }
    }

    it('keeps the newest endpoint answer when an older read returns last', async () => {
      const older = deferred<unknown>()
      const newer = deferred<unknown>()
      vi.mocked(listEnvironmentEndpoints)
        .mockReturnValueOnce(older.promise as never)
        .mockReturnValueOnce(newer.promise as never)
      const access = useEnvironmentAccess(ref(environmentId), ref(1), ref('project-1'))

      const first = access.loadEndpoints(true)
      const second = access.loadEndpoints(true)
      newer.resolve({ data: { items: [endpoint('pending')] } })
      await second
      older.resolve({ data: { items: [endpoint('healthy')] } })
      await first

      expect(access.endpoints.kind).toBe('success')
      if (access.endpoints.kind === 'success') expect(access.endpoints.data[0].health).toBe('pending')
    })

    it('does not let an older "active" grant read overwrite a newer read that found no grant', async () => {
      const active = grant('active')
      const olderList = deferred<unknown>()
      vi.mocked(listEnvironmentAccessGrants)
        .mockReturnValueOnce(olderList.promise as never)
        .mockResolvedValueOnce({ data: { items: [] } } as never)
      vi.mocked(getAccessGrant).mockResolvedValue({ data: active } as never)
      const access = useEnvironmentAccess(ref(active.environmentId), ref(1), ref('project-1'))

      const first = access.loadCurrentGrant(true)
      await expect(access.loadCurrentGrant(true)).resolves.toEqual({ ok: true })
      expect(access.grant.kind).toBe('idle')

      olderList.resolve({ data: { items: [{ id: active.id }] } })
      await expect(first).resolves.toEqual({ ok: false })
      expect(access.grant.kind).toBe('idle')
    })

    it('drops an older grant detail read that resolves after a newer "expired" answer', async () => {
      const active = grant('active')
      const olderDetail = deferred<unknown>()
      vi.mocked(listEnvironmentAccessGrants).mockResolvedValue({ data: { items: [{ id: active.id }] } } as never)
      vi.mocked(getAccessGrant)
        .mockReturnValueOnce(olderDetail.promise as never)
        .mockResolvedValueOnce({ data: { ...active, state: 'expired' } } as never)
      const access = useEnvironmentAccess(ref(active.environmentId), ref(1), ref('project-1'))

      const first = access.loadCurrentGrant(true)
      // Let the first read reach its detail request before the newer read starts.
      await vi.waitFor(() => expect(getAccessGrant).toHaveBeenCalledTimes(1))
      await access.loadCurrentGrant(true)
      expect(access.grant.kind).toBe('error')

      olderDetail.resolve({ data: active })
      await first
      expect(access.grant.kind).toBe('error')
      if (access.grant.kind === 'error') expect(access.grant.diagnostic.code).toBe('ACCESS_GRANT_AUTHORITY_DRIFT')
    })

    it('keeps a confirmed revocation when a read started before it returns "active" afterwards', async () => {
      const active = grant('active')
      vi.mocked(listEnvironmentAccessGrants).mockResolvedValueOnce({ data: { items: [{ id: active.id }] } } as never)
      vi.mocked(getAccessGrant).mockResolvedValueOnce({ data: active } as never)
      const access = useEnvironmentAccess(ref(active.environmentId), ref(1), ref('project-1'))
      await access.loadCurrentGrant()
      expect(access.grant.kind).toBe('success')

      const inFlight = deferred<unknown>()
      vi.mocked(listEnvironmentAccessGrants).mockReturnValueOnce(inFlight.promise as never)
      vi.mocked(getAccessGrant).mockResolvedValueOnce({ data: active } as never)
      vi.mocked(revokeAccessGrant).mockResolvedValue({ data: { ...active, state: 'revoked' } } as never)
      const pollRead = access.loadCurrentGrant(true)
      await expect(access.revokeGrant()).resolves.toEqual({ ok: true })
      expect(access.grant.kind).toBe('revoked')

      inFlight.resolve({ data: { items: [{ id: active.id }] } })
      await pollRead
      expect(access.grant.kind).toBe('revoked')
    })

    it('stops following an issued grant once a newer read has taken over', async () => {
      vi.useFakeTimers()
      vi.mocked(listEnvironmentEndpoints).mockResolvedValue({ data: { items: [endpoint('healthy')] } } as never)
      vi.mocked(createAccessGrant).mockResolvedValue({ data: grant('requested') } as never)
      vi.mocked(getAccessGrant).mockResolvedValue({ data: grant('active') } as never)
      vi.mocked(listEnvironmentAccessGrants).mockResolvedValue({ data: { items: [] } } as never)
      const access = useEnvironmentAccess(ref(environmentId), ref(1), ref('project-1'))
      await access.loadEndpoints()

      const issuing = access.createGrant()
      await vi.advanceTimersByTimeAsync(0)
      access.resetGrant()
      await vi.advanceTimersByTimeAsync(500)
      await expect(issuing).resolves.toEqual({ ok: false })
      expect(access.grant.kind).toBe('idle')
    })
  })
})
