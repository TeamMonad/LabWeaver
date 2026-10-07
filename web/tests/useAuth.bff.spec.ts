import { afterEach, describe, expect, it, vi } from 'vitest'

describe('BFF browser session', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
    vi.unstubAllEnvs()
    vi.resetModules()
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
