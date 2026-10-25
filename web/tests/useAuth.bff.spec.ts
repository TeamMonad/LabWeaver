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
        actorId: '01900000-0000-7000-8000-000000000001',
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
      .mockResolvedValueOnce(sessionOf(actorA))
      .mockResolvedValueOnce(new Response(JSON.stringify({ csrfToken: 'csrf-token', actorId: actorA }), { status: 200 }))
      .mockReturnValueOnce(logoutResponse)
    const navigate = vi.fn()
    vi.stubGlobal('fetch', fetch)

    const { useAuth } = await import('@/composables/useAuth')
    const auth = useAuth(navigate)
    await auth.loadUser()
    const first = auth.logout()
    const second = auth.logout()
    await vi.waitFor(() => expect(fetch.mock.calls.map(([url]) => url)).toEqual(['/api/v1/auth/session', '/api/v1/auth/csrf', '/auth/logout']))

    resolveLogout(new Response(JSON.stringify({ diagnosticCode: 'LW_AUTH_SESSION_REJECTED' }), { status: 401 }))
    await first
    await second

    // The second call sent nothing of its own.
    expect(fetch).toHaveBeenCalledTimes(3)
    expect(navigate).not.toHaveBeenCalled()
    expect(auth.error.value?.message).toBe('登录已失效，请重新登录')
  })

  it('refuses a logout whose CSRF token was issued to another actor without sending it', async () => {
    vi.stubEnv('VITE_API_AUTH_MODE', 'bff')
    const fetch = vi.fn()
      .mockResolvedValueOnce(sessionOf(actorA))
      .mockResolvedValueOnce(new Response(JSON.stringify({ csrfToken: 'token-of-b', actorId: actorB }), { status: 200 }))
      .mockResolvedValue(sessionOf(actorB))
    const navigate = vi.fn()
    vi.stubGlobal('fetch', fetch)

    const { useAuth } = await import('@/composables/useAuth')
    const auth = useAuth(navigate)
    await auth.loadUser()
    await auth.logout()

    expect(fetch.mock.calls.map(([url]) => url)).not.toContain('/auth/logout')
    expect(navigate).not.toHaveBeenCalled()
    expect(auth.error.value?.message).toBe('此浏览器的登录账号已变化，未执行退出。请刷新页面后再操作。')
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

  it('sends no logout for actor A while the rejection read is held, and refuses it once actor B is confirmed', async () => {
    let answerRead!: (response: Response) => void
    const fetch = vi.fn()
      .mockResolvedValueOnce(sessionOf(actorA))
      .mockReturnValueOnce(new Promise<Response>((resolve) => { answerRead = resolve }))
    vi.stubGlobal('fetch', fetch)
    const navigate = vi.fn()
    const { useAuth } = await import('@/composables/useAuth')
    const session = await import('@/api/session')
    const auth = useAuth(navigate)
    await auth.loadUser()

    // The cookie now belongs to actor B; the rejection read has not answered yet.
    session.reportSessionRejected()
    const logout = auth.logout()
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(fetch.mock.calls.map(([url]) => url)).toEqual(['/api/v1/auth/session', '/api/v1/auth/session'])

    answerRead(sessionOf(actorB))
    await logout

    const urls = fetch.mock.calls.map(([url]) => url)
    expect(urls).not.toContain('/api/v1/auth/csrf')
    expect(urls).not.toContain('/auth/logout')
    expect(session.sessionActor()).toBe(actorB)
    expect(navigate).not.toHaveBeenCalled()
    expect(auth.error.value?.message).toBe('此浏览器的登录账号已变化，未执行退出。请刷新页面后再操作。')
  })

  it('ends a hung session read after its timeout and refuses the waiting write unsent, keeping the actor\'s commands', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    try {
      const fetch = vi.fn()
        .mockResolvedValueOnce(sessionOf(actorA))
        // The re-read never answers; only its abort ends it.
        .mockImplementationOnce((_url: string, init: RequestInit) => new Promise<Response>((_resolve, reject) => {
          init.signal?.addEventListener('abort', () => reject(new DOMException('The operation was aborted.', 'AbortError')))
        }))
      vi.stubGlobal('fetch', fetch)
      const { useAuth, SESSION_READ_TIMEOUT_MS } = await import('@/composables/useAuth')
      const session = await import('@/api/session')
      const client = await import('@/api/client')
      const commands = await import('@/utils/pendingCommands')
      const auth = useAuth()
      await auth.loadUser()
      const draft = { fingerprint: 'draft', idempotencyKey: 'draft-key', data: null }
      commands.writePendingCommand(actorA, 'spec:draft', draft)
      const api = client.createLabWeaverApiClient({
        baseUrl: '/',
        authentication: { mode: 'bff', csrfToken: async () => ({ token: 'token-of-a', actorId: actorA }) },
      })
      const adapter = vi.fn(async (config) => ({ data: {}, status: 200, statusText: 'OK', headers: {}, config }))
      api.instance.defaults.adapter = adapter

      session.reportSessionRejected()
      const write = api.post({ url: '/api/v1/projects', body: { name: '项目' } })
      await vi.advanceTimersByTimeAsync(SESSION_READ_TIMEOUT_MS)
      const result = await write

      expect(result).toMatchObject({ diagnosticCode: 'LW_SDK_IDENTITY_UNCONFIRMED' })
      expect(adapter).not.toHaveBeenCalled()
      expect(session.sessionActorInDoubt.value).toBe(true)
      expect(auth.isAuthenticated.value).toBe(false)
      // The unconfirmed command stays for a manual resubmission.
      expect(commands.readPendingCommand(actorA, 'spec:draft')).toEqual(draft)
    } finally {
      vi.useRealTimers()
    }
  })

  /**
   * Answers fetches by URL: each session read takes the next queued answer,
   * the CSRF lookup returns `csrf`, and every POST is recorded.
   */
  function routedFetch(sessionReads: Array<() => Promise<Response>>, csrf: () => Promise<Response>) {
    const posts: string[] = []
    const fetch = vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') {
        posts.push(url)
        return new Response(JSON.stringify({ logoutUrl: 'https://identity.example.test/logout' }), { status: 200 })
      }
      if (url === '/api/v1/auth/csrf') return csrf()
      const next = sessionReads.shift()
      if (!next) throw new TypeError('Failed to fetch')
      return next()
    })
    vi.stubGlobal('fetch', fetch)
    return { fetch, posts }
  }

  const csrfOf = (actorId: string) => new Response(JSON.stringify({ csrfToken: `token-of-${actorId}`, actorId }), { status: 200 })
  const failedRead = async (): Promise<Response> => { throw new TypeError('Failed to fetch') }

  const LOGOUT_TOKEN_INVALID = '暂时无法确认退出所需的安全令牌，本次退出没有执行。请稍后重试。'
  const invalidCsrfAnswers: Array<[string, () => Response]> = [
    ['malformed JSON', () => new Response('{"csrfToken":', { status: 200 })],
    ['no csrfToken', () => new Response(JSON.stringify({ actorId: actorA }), { status: 200 })],
    ['a token of the wrong shape', () => new Response(JSON.stringify({ csrfToken: 42, actorId: actorA }), { status: 200 })],
  ]

  it('(c) sends no logout when the actor goes A→B→A while the logout waits for its CSRF token', async () => {
    let answerCsrf!: (response: Response) => void
    const { posts } = routedFetch(
      [async () => sessionOf(actorA), async () => sessionOf(actorB), async () => sessionOf(actorA)],
      () => new Promise<Response>((resolve) => { answerCsrf = resolve }),
    )
    const navigate = vi.fn()
    const { useAuth } = await import('@/composables/useAuth')
    const auth = useAuth(navigate)
    await auth.loadUser()

    const logout = auth.logout()
    await vi.waitFor(() => expect(answerCsrf).toBeTypeOf('function'))
    // Another tab signs in as B, then as A again: same actor, different session.
    await auth.loadUser()
    await auth.loadUser()
    answerCsrf(csrfOf(actorA))
    await logout

    expect(posts).toEqual([])
    expect(navigate).not.toHaveBeenCalled()
    expect(auth.error.value?.message).toBe('此浏览器的登录账号已变化，未执行退出。请刷新页面后再操作。')
  })

  describe('ending the browser session while the identity is unknown', () => {
    /** Actor A was confirmed, then the session read failed: the identity is unknown. */
    async function unknownIdentity(csrf: () => Promise<Response>) {
      const routed = routedFetch([async () => sessionOf(actorA), failedRead], csrf)
      const navigate = vi.fn()
      const { useAuth } = await import('@/composables/useAuth')
      const auth = useAuth(navigate)
      await auth.loadUser()
      await auth.loadUser()
      expect(auth.identityUnknown.value).toBe(true)
      // The normal logout refuses while the identity is unknown.
      await auth.logout()
      expect(routed.posts).toEqual([])
      return { ...routed, auth, navigate }
    }

    it('signs out when the fresh CSRF token belongs to the actor this page belongs to', async () => {
      const { auth, posts, navigate } = await unknownIdentity(async () => csrfOf(actorA))

      await auth.endBrowserSession()

      expect(posts).toEqual(['/auth/logout'])
      expect(navigate).toHaveBeenCalledWith('https://identity.example.test/logout')
      expect(auth.identityUnknown.value).toBe(false)
      expect(auth.error.value).toBeNull()
    })

    it('sends nothing when the fresh CSRF token belongs to another actor', async () => {
      const { auth, posts, navigate } = await unknownIdentity(async () => csrfOf(actorB))

      await auth.endBrowserSession()

      expect(posts).toEqual([])
      expect(navigate).not.toHaveBeenCalled()
      expect(auth.error.value?.message).toBe('此浏览器的登录账号已变化，未执行退出。请刷新页面后再操作。')
    })

    it('sends nothing when the CSRF lookup fails too', async () => {
      const { auth, posts, navigate } = await unknownIdentity(async () => { throw new TypeError('Failed to fetch') })

      await auth.endBrowserSession()

      expect(posts).toEqual([])
      expect(navigate).not.toHaveBeenCalled()
      expect(auth.error.value?.message).toBe('暂时无法连接登录服务，本次退出没有执行。请检查网络后重试。')
    })

    it.each(invalidCsrfAnswers)('sends nothing and names no account for a 200 CSRF answer with %s', async (_case, answer) => {
      const { auth, posts, navigate } = await unknownIdentity(async () => answer())

      await auth.endBrowserSession()

      expect(posts).toEqual([])
      expect(navigate).not.toHaveBeenCalled()
      expect(auth.error.value?.message).toBe(LOGOUT_TOKEN_INVALID)
    })
  })

  it.each(invalidCsrfAnswers)('refuses the logout of a confirmed actor readably for a 200 CSRF answer with %s', async (_case, answer) => {
    const { posts } = routedFetch([async () => sessionOf(actorA)], async () => answer())
    const navigate = vi.fn()
    const { useAuth } = await import('@/composables/useAuth')
    const auth = useAuth(navigate)
    await auth.loadUser()

    await auth.logout()

    expect(posts).toEqual([])
    expect(navigate).not.toHaveBeenCalled()
    expect(auth.error.value?.message).toBe(LOGOUT_TOKEN_INVALID)
    // An unreadable answer says nothing about who holds the session.
    expect(auth.isAuthenticated.value).toBe(true)
  })
})
