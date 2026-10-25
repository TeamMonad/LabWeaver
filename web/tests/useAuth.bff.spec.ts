import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
// Each test re-imports useAuth after vi.resetModules() so the BFF mode and the
// module-level session state start fresh. Loading the graph once here moves
// the cold transform (oidc-client-ts, config, SDK types) into file collection,
// which has no per-test timeout; the per-test re-import only re-evaluates.
import '@/composables/useAuth'

describe('BFF browser session', () => {
  beforeEach(() => {
    // Drop the warm-up instance so every test evaluates useAuth under its own env stub.
    vi.resetModules()
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.unstubAllEnvs()
  })

  it('loads the safe actor, role, and course context without browser OIDC configuration', async () => {
    // This spec asserts BFF session semantics itself, so it pins the auth mode
    // explicitly instead of inheriting the mode from the test environment.
    vi.stubEnv('VITE_API_AUTH_MODE', 'bff')
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({
      actor: {
        actorId: '01900000-0000-7000-8000-000000000001',
        roles: ['teacher'],
        expiresAt: '2099-01-01T00:00:00.000Z',
      },
      authorizationRevision: 2,
      expiresAt: '2099-01-01T00:00:00.000Z',
      scopes: [
        { kind: 'global' },
        { kind: 'course', course_id: '01900000-0000-7000-8000-000000000002' },
      ],
    }), { status: 200, headers: { 'content-type': 'application/json' } }))
    vi.stubGlobal('fetch', fetch)

    const { useAuth } = await import('@/composables/useAuth')
    const auth = useAuth()
    await auth.loadUser()

    expect(fetch).toHaveBeenCalledWith('/api/v1/auth/session', expect.objectContaining({
      credentials: 'include',
    }))
    expect(auth.isAuthenticated.value).toBe(true)
    expect(auth.user.value?.profile).toMatchObject({
      actor_id: '01900000-0000-7000-8000-000000000001',
      roles: ['teacher'],
      course_id: '01900000-0000-7000-8000-000000000002',
    })
  })

  it('revokes the BFF session and top-level navigates to the exact provider logout URL', async () => {
    vi.stubEnv('VITE_API_AUTH_MODE', 'bff')
    const logoutUrl = 'https://identity.example.test/realms/labweaver/logout?id_token_hint=opaque'
    const responses = [
      new Response(JSON.stringify({
        actor: {
          actorId: '01900000-0000-7000-8000-000000000001',
          roles: ['teacher'],
          expiresAt: '2099-01-01T00:00:00.000Z',
        },
        authorizationRevision: 2,
        expiresAt: '2099-01-01T00:00:00.000Z',
        scopes: [{ kind: 'global' }],
      }), { status: 200, headers: { 'content-type': 'application/json' } }),
      new Response(JSON.stringify({
        csrfToken: 'csrf-token',
        expiresAt: '2099-01-01T00:00:00.000Z',
      }), { status: 200, headers: { 'content-type': 'application/json' } }),
      new Response(JSON.stringify({ logoutUrl }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    ]
    const fetch = vi.fn().mockImplementation(async () => responses.shift())
    const navigate = vi.fn()
    vi.stubGlobal('fetch', fetch)

    const { useAuth } = await import('@/composables/useAuth')
    const auth = useAuth(navigate)
    await auth.loadUser()
    await auth.logout()

    expect(fetch).toHaveBeenCalledWith('/auth/logout', expect.objectContaining({
      method: 'POST',
      credentials: 'include',
      headers: {
        Accept: 'application/json, application/problem+json',
        'X-CSRF-Token': 'csrf-token',
      },
    }))
    expect(navigate).toHaveBeenCalledWith(logoutUrl)
    expect(auth.user.value).toBeNull()
    expect(auth.error.value).toBeNull()
  })

  it('clears stale auth state when the CSRF lookup reports an expired session', async () => {
    vi.stubEnv('VITE_API_AUTH_MODE', 'bff')
    const responses = [
      new Response(JSON.stringify({
        actor: {
          actorId: '01900000-0000-7000-8000-000000000001',
          roles: ['teacher'],
          expiresAt: '2099-01-01T00:00:00.000Z',
        },
        authorizationRevision: 2,
        expiresAt: '2099-01-01T00:00:00.000Z',
        scopes: [{ kind: 'global' }],
      }), { status: 200, headers: { 'content-type': 'application/json' } }),
      new Response(JSON.stringify({ diagnosticCode: 'LW_AUTH_SESSION_REJECTED' }), { status: 401 }),
    ]
    const fetch = vi.fn().mockImplementation(async () => responses.shift())
    const navigate = vi.fn()
    vi.stubGlobal('fetch', fetch)

    const { useAuth } = await import('@/composables/useAuth')
    const auth = useAuth(navigate)
    await auth.loadUser()
    await auth.logout()

    expect(fetch).toHaveBeenCalledTimes(2)
    expect(fetch.mock.calls.map(([url]) => url)).toEqual([
      '/api/v1/auth/session',
      '/api/v1/auth/csrf',
    ])
    expect(auth.user.value).toBeNull()
    expect(auth.isAuthenticated.value).toBe(false)
    expect(auth.error.value?.message).toBe('登录已失效，请重新登录')
    expect((auth.error.value as Error & { cause?: string })?.cause).toBe(
      'BFF CSRF lookup failed with HTTP 401',
    )
    expect(navigate).not.toHaveBeenCalled()
  })

  it('keeps logout single-flight and exposes a failed BFF response', async () => {
    vi.stubEnv('VITE_API_AUTH_MODE', 'bff')
    let resolveLogout!: (response: Response) => void
    const logoutResponse = new Promise<Response>((resolve) => { resolveLogout = resolve })
    const fetch = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({ csrfToken: 'csrf-token' }), { status: 200 }))
      .mockReturnValueOnce(logoutResponse)
    const navigate = vi.fn()
    vi.stubGlobal('fetch', fetch)

    const { useAuth } = await import('@/composables/useAuth')
    const auth = useAuth(navigate)
    const first = auth.logout()
    const second = auth.logout()
    await Promise.resolve()
    expect(fetch).toHaveBeenCalledTimes(1)

    resolveLogout(new Response(JSON.stringify({ diagnosticCode: 'LW_AUTH_SESSION_REJECTED' }), { status: 401 }))
    await first
    await second

    expect(navigate).not.toHaveBeenCalled()
    expect(auth.error.value?.message).toBe('登录已失效，请重新登录')
  })
})

function sessionOf(actorId: string, expiresAt = '2099-01-01T00:00:00.000Z') {
  return new Response(JSON.stringify({
    actor: { actorId, roles: ['student'], expiresAt },
    authorizationRevision: 1,
    expiresAt,
    scopes: [{ kind: 'global' }],
  }), { status: 200, headers: { 'content-type': 'application/json' } })
}

const actorA = '01900000-0000-7000-8000-00000000000a'
const actorB = '01900000-0000-7000-8000-00000000000b'

describe('BFF session epoch (#198)', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.stubEnv('VITE_API_AUTH_MODE', 'bff')
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.unstubAllEnvs()
  })

  it('advances only when another actor signs in or the session is confirmed ended', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(sessionOf(actorA))
      .mockResolvedValueOnce(sessionOf(actorA))
      .mockResolvedValueOnce(new Response('', { status: 503 }))
      .mockResolvedValueOnce(sessionOf(actorA))
      .mockResolvedValueOnce(sessionOf(actorB))
      .mockResolvedValueOnce(new Response(JSON.stringify({ diagnosticCode: 'LW_AUTH_SESSION_REJECTED' }), { status: 401 }))
    vi.stubGlobal('fetch', fetch)
    const { useAuth } = await import('@/composables/useAuth')
    const { sessionEpoch } = await import('@/api/session')
    const auth = useAuth()
    const epochs: number[] = []
    for (let read = 0; read < 6; read += 1) {
      await auth.loadUser()
      epochs.push(sessionEpoch.value)
    }

    // First sign-in, same actor again, temporarily unknown, same actor, actor B, signed out.
    expect(epochs).toEqual([0, 0, 0, 0, 1, 2])
    expect(auth.user.value).toBeNull()
  })

  it('ends the epoch when the session expires and starts another once it is valid again', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(sessionOf(actorA))
      .mockResolvedValueOnce(sessionOf(actorA, '2000-01-01T00:00:00.000Z'))
      .mockResolvedValueOnce(sessionOf(actorA))
    vi.stubGlobal('fetch', fetch)
    const { useAuth } = await import('@/composables/useAuth')
    const { sessionEpoch, sessionActor } = await import('@/api/session')
    const auth = useAuth()
    const epochs: number[] = []
    for (let read = 0; read < 3; read += 1) {
      await auth.loadUser()
      epochs.push(sessionEpoch.value)
    }

    // Signed in, expired (same actor), valid again.
    expect(epochs).toEqual([0, 1, 2])
    expect(auth.isAuthenticated.value).toBe(true)
    expect(sessionActor()).toBe(actorA)
  })

  it('does not let a session read that started earlier overwrite the newer actor', async () => {
    let answerEarlierRead!: (response: Response) => void
    const fetch = vi.fn()
      .mockResolvedValueOnce(sessionOf(actorA))
      .mockReturnValueOnce(new Promise<Response>((resolve) => { answerEarlierRead = resolve }))
      .mockResolvedValueOnce(sessionOf(actorB))
    vi.stubGlobal('fetch', fetch)
    const { useAuth } = await import('@/composables/useAuth')
    const { sessionEpoch } = await import('@/api/session')
    const auth = useAuth()
    await auth.loadUser()

    const earlier = auth.loadUser()
    await auth.loadUser()
    expect(auth.user.value?.profile).toMatchObject({ actor_id: actorB })
    answerEarlierRead(sessionOf(actorA))
    await earlier

    expect(auth.user.value?.profile).toMatchObject({ actor_id: actorB })
    expect(sessionEpoch.value).toBe(1)
  })

  it('re-reads the actor when the transport reports a CSRF rejection and ends the epoch for another actor', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(sessionOf(actorA))
      .mockResolvedValueOnce(sessionOf(actorB))
    vi.stubGlobal('fetch', fetch)
    const { useAuth } = await import('@/composables/useAuth')
    const session = await import('@/api/session')
    const auth = useAuth()
    await auth.loadUser()
    expect(session.sessionActor()).toBe(actorA)

    session.reportSessionRejected()
    await vi.waitFor(() => expect(auth.user.value?.profile).toMatchObject({ actor_id: actorB }))

    expect(fetch).toHaveBeenLastCalledWith('/api/v1/auth/session', expect.objectContaining({ credentials: 'include' }))
    expect(session.sessionActor()).toBe(actorB)
    expect(session.sessionEpoch.value).toBe(1)
  })
})
