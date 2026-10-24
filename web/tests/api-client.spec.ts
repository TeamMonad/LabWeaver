import axios, { AxiosError, AxiosHeaders } from 'axios'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createLabWeaverApiClient } from '@/api/client'
import { diagnosticFromError, extractProblemDetails, makeDiagnostic } from '@/types/async'

vi.mock('@/config', () => ({
  API_AUTH_MODE: 'bearer',
  API_BASE_URL: '/',
}))

vi.mock('@/composables/useAuth', () => ({
  getOidcAccessToken: vi.fn(async () => 'configured-oidc-token'),
}))

function makeClient(baseUrl: string, authentication: Parameters<typeof createLabWeaverApiClient>[0]['authentication']) {
  return createLabWeaverApiClient({ baseUrl, authentication })
}

function installSuccessfulAdapter(client: ReturnType<typeof createLabWeaverApiClient>) {
  const adapter = vi.fn(async (config) => ({
    data: { ok: true },
    status: 200,
    statusText: 'OK',
    headers: {},
    config,
  }))
  client.instance.defaults.adapter = adapter
  return adapter
}

afterEach(() => {
  vi.restoreAllMocks()
})

describe('createLabWeaverApiClient', () => {
  it('builds root-origin paths with one leading slash and preserves bearer auth', async () => {
    const accessToken = vi.fn(async () => 'root-oidc-token')
    const client = makeClient('/', { mode: 'bearer', accessToken })

    expect(client.buildUrl({ url: '/api/v1/resource/gpu-catalog' })).toBe('/api/v1/resource/gpu-catalog')

    const adapter = installSuccessfulAdapter(client)
    await client.get({ url: '/api/v1/resource/gpu-catalog' })

    expect(accessToken).toHaveBeenCalledOnce()
    expect(adapter).toHaveBeenCalledOnce()
    const requestConfig = adapter.mock.calls[0][0]
    expect(requestConfig.url).toBe('/api/v1/resource/gpu-catalog')
    expect(requestConfig.headers.Authorization).toBe('Bearer root-oidc-token')
  })

  it('trims trailing slashes from an absolute API origin while keeping its host', () => {
    const client = makeClient('https://api.example.test///', {
      mode: 'bearer',
      accessToken: async () => 'absolute-oidc-token',
    })

    expect(client.buildUrl({ url: '/api/v1/resource/gpu-catalog' })).toBe(
      'https://api.example.test/api/v1/resource/gpu-catalog',
    )
  })

  it('uses a root-relative CSRF URL for BFF requests', async () => {
    const csrfGet = vi.spyOn(axios, 'get').mockResolvedValue({
      data: {
        csrfToken: 'csrf-token',
        actorId: 'actor-a',
        expiresAt: new Date(Date.now() + 120_000).toISOString(),
      },
    } as never)
    const client = makeClient('/', { mode: 'bff' })
    const adapter = installSuccessfulAdapter(client)

    await client.post({ url: '/api/v1/resource-requests', body: {} })

    expect(csrfGet).toHaveBeenCalledWith(
      '/api/v1/auth/csrf',
      expect.objectContaining({ baseURL: '' }),
    )
    expect(adapter.mock.calls[0][0].url).toBe('/api/v1/resource-requests')
    expect(adapter.mock.calls[0][0].headers['X-CSRF-Token']).toBe('csrf-token')
  })

  it('rejects a contract-prefixed API base URL', () => {
    expect(() => makeClient('/api/v1', { mode: 'bearer', accessToken: async () => 'token' })).toThrowError(
      'Public API base URL must identify the origin, not repeat the /api/v1 contract prefix.',
    )
  })
})

describe('x-request-id fallback', () => {
  function installErrorAdapter(client: ReturnType<typeof createLabWeaverApiClient>, data: unknown, headers: Record<string, string>) {
    client.instance.defaults.adapter = vi.fn(async (config) => {
      const response = { data, status: 403, statusText: 'Forbidden', headers: new AxiosHeaders(headers), config }
      throw new AxiosError('Request failed with status code 403', AxiosError.ERR_BAD_REQUEST, config, null, response as never)
    })
  }

  it('shows the response header request id when the error body has none', async () => {
    const client = makeClient('/', { mode: 'bearer', accessToken: async () => 'token' })
    installErrorAdapter(client, { diagnosticCode: 'LW_ACCESS_FORBIDDEN', detail: 'forbidden' }, { 'X-Request-Id': 'req-from-header' })
    const result = await client.get({ url: '/api/v1/projects/p' })
    const fromBody = makeDiagnostic('LW_ACCESS_FORBIDDEN', 'forbidden', false, extractProblemDetails(result.error))
    expect(fromBody.requestId).toBe('req-from-header')
    expect(diagnosticFromError(result.error, 'X', 'x').requestId).toBe('req-from-header')
  })

  it('prefers the RFC 9457 body request id over the header', async () => {
    const client = makeClient('/', { mode: 'bearer', accessToken: async () => 'token' })
    installErrorAdapter(
      client,
      { diagnosticCode: 'LW_PROJECT_FORBIDDEN', detail: 'forbidden', requestId: 'req-from-body', status: 403 },
      { 'x-request-id': 'req-from-header' },
    )
    const result = await client.get({ url: '/api/v1/projects/p' })
    expect(diagnosticFromError(result.error, 'X', 'x').requestId).toBe('req-from-body')
  })

  it('never invents a request id when the server sent none', async () => {
    const client = makeClient('/', { mode: 'bearer', accessToken: async () => 'token' })
    installErrorAdapter(client, { diagnosticCode: 'LW_ACCESS_FORBIDDEN', detail: 'forbidden' }, {})
    const result = await client.get({ url: '/api/v1/projects/p' })
    expect(diagnosticFromError(result.error, 'X', 'x').requestId).toBeUndefined()
    expect(makeDiagnostic('LW_ACCESS_FORBIDDEN', 'forbidden', false, { status: 403 }).requestId).toBeUndefined()
  })
})

const future = () => new Date(Date.now() + 120_000).toISOString()

const csrfRejected = {
  type: 'https://labweaver.dev/problems/access',
  title: 'Access request blocked',
  status: 403,
  detail: 'The request was rejected by an access boundary.',
  instance: 'urn:labweaver:request:csrf-1',
  diagnosticCode: 'LW_AUTH_CSRF_REJECTED',
  requestId: 'csrf-1',
  retryable: false,
}

/** `Promise.withResolvers` is outside the project's ES2020 lib, so the executor form captures the resolver. */
function deferred() {
  let resolve!: () => void
  const promise = new Promise<void>((settle) => { resolve = settle })
  return { promise, resolve }
}

/**
 * Session state is module-level: every test starts from a fresh epoch and no
 * confirmation. Static imports would keep the first module instances, so the
 * modules are imported again after `vi.resetModules()`.
 */
async function freshTransport() {
  vi.resetModules()
  const session = await import('@/api/session')
  const client = await import('@/api/client')
  const { default: freshAxios } = await import('axios')
  return { session, client, axios: freshAxios }
}

describe('browser session changes (#198)', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  const csrfFor = (actorId: string, csrfToken: string) => ({ data: { csrfToken, actorId, expiresAt: future() } }) as never

  it('does not replay a CSRF-rejected write, re-reads the actor and lets the user resubmit with a new token', async () => {
    const { session, client, axios: transportAxios } = await freshTransport()
    session.recordSessionActor('actor-a')
    const csrfGet = vi.spyOn(transportAxios, 'get')
      .mockResolvedValueOnce(csrfFor('actor-a', 'token-old-session'))
      .mockResolvedValueOnce(csrfFor('actor-a', 'token-current-session'))
    const confirm = vi.fn(async () => undefined)
    session.registerSessionConfirmation(confirm)
    const api = client.createLabWeaverApiClient({ baseUrl: '/', authentication: { mode: 'bff' } })
    const sent: { token: unknown; body: unknown }[] = []
    api.instance.defaults.adapter = vi.fn(async (config) => {
      sent.push({ token: config.headers['X-CSRF-Token'], body: JSON.parse(String(config.data)) })
      if (sent.length === 1) {
        const response = { data: csrfRejected, status: 403, statusText: 'Forbidden', headers: new AxiosHeaders(), config }
        throw new AxiosError('Request failed with status code 403', AxiosError.ERR_BAD_REQUEST, config, null, response as never)
      }
      return { data: { ok: true }, status: 201, statusText: 'Created', headers: {}, config }
    })

    const failed = await api.post({ url: '/api/v1/projects', body: { name: '第一次提交' } })

    expect(failed.error).toMatchObject({ diagnosticCode: 'LW_AUTH_CSRF_REJECTED' })
    const diagnostic = diagnosticFromError(failed.error, 'PROJECT_CREATE_FAILED', '创建项目失败')
    expect(diagnostic.message).toBe('登录会话已在其他页面变化或安全校验已失效，这次操作没有执行。')
    expect(diagnostic.nextStep).toContain('手动重新提交')
    expect(diagnostic.nextStep).toContain('不会自动重试')
    expect(diagnostic.requestId).toBe('csrf-1')
    expect(sent).toHaveLength(1)
    expect(confirm).toHaveBeenCalledOnce()

    // Only the user's own resubmission sends the content again, with a token fetched after the rejection.
    expect((await api.post({ url: '/api/v1/projects', body: { name: '手动重新提交' } })).error).toBeUndefined()
    expect(sent).toEqual([
      { token: 'token-old-session', body: { name: '第一次提交' } },
      { token: 'token-current-session', body: { name: '手动重新提交' } },
    ])
    expect(csrfGet).toHaveBeenCalledTimes(2)
  })

  it('does not send a write when the BFF issues its CSRF token to another actor', async () => {
    const { session, client, axios: transportAxios } = await freshTransport()
    session.recordSessionActor('actor-a')
    vi.spyOn(transportAxios, 'get').mockResolvedValue(csrfFor('actor-b', 'token-of-b'))
    const confirm = vi.fn(async () => undefined)
    session.registerSessionConfirmation(confirm)
    const api = client.createLabWeaverApiClient({ baseUrl: '/', authentication: { mode: 'bff' } })
    const adapter = installSuccessfulAdapter(api)

    const result = await api.post({ url: '/api/v1/users/me/ssh-keys', body: { publicKey: 'A 的公钥' } })

    expect(result).toMatchObject({ diagnosticCode: 'LW_SDK_SESSION_CHANGED' })
    expect(result.data).toBeUndefined()
    expect(adapter).not.toHaveBeenCalled()
    // The cookie changed hands without this tab noticing: the actor is read again.
    expect(confirm).toHaveBeenCalledOnce()
  })

  it('rejects a CSRF response without the issuing actor instead of sending an unbound token', async () => {
    const { session, client, axios: transportAxios } = await freshTransport()
    session.recordSessionActor('actor-a')
    vi.spyOn(transportAxios, 'get').mockResolvedValue({ data: { csrfToken: 'token', expiresAt: future() } } as never)
    const api = client.createLabWeaverApiClient({ baseUrl: '/', authentication: { mode: 'bff' } })
    const adapter = installSuccessfulAdapter(api)

    const result = await api.post({ url: '/api/v1/projects', body: { name: '项目' } })

    expect(result).toMatchObject({ diagnosticCode: 'LW_SDK_PROBLEM_INVALID' })
    expect(adapter).not.toHaveBeenCalled()
  })

  it('never sends a waiting write once the actor changed while its token was fetched', async () => {
    const { session, client, axios: transportAxios } = await freshTransport()
    session.recordSessionActor('actor-a')
    let answerCsrf!: (value: unknown) => void
    vi.spyOn(transportAxios, 'get').mockReturnValueOnce(new Promise((resolve) => { answerCsrf = resolve }) as never)
    const api = client.createLabWeaverApiClient({ baseUrl: '/', authentication: { mode: 'bff' } })
    const adapter = installSuccessfulAdapter(api)

    const staleForm = api.post({ url: '/api/v1/users/me/ssh-keys', body: { publicKey: 'A 的公钥' } })
    await new Promise((resolve) => setTimeout(resolve, 0))
    session.recordSessionActor('actor-b')
    answerCsrf(csrfFor('actor-b', 'token-of-b'))

    const result = await staleForm
    expect(result).toMatchObject({ diagnosticCode: 'LW_SDK_SESSION_CHANGED' })
    expect(result.data).toBeUndefined()
    expect(adapter).not.toHaveBeenCalled()
  })

  it('discards a response the previous actor requested once the session epoch advances', async () => {
    const { session, client } = await freshTransport()
    const api = client.createLabWeaverApiClient({ baseUrl: '/', authentication: { mode: 'bearer', accessToken: async () => 'token' } })
    const response = deferred()
    const adapter = vi.fn(async (config) => {
      await response.promise
      return { data: [{ id: 'project-of-a' }], status: 200, statusText: 'OK', headers: {}, config }
    })
    api.instance.defaults.adapter = adapter

    const listed = api.get({ url: '/api/v1/projects' })
    await vi.waitFor(() => expect(adapter).toHaveBeenCalledOnce())
    session.advanceSessionEpoch()
    response.resolve()

    const result = await listed
    expect(result).toMatchObject({ diagnosticCode: 'LW_SDK_SESSION_CHANGED' })
    expect(result.data).toBeUndefined()
  })

  it('discards an error the previous actor received instead of reporting it for the new session', async () => {
    const { session, client } = await freshTransport()
    const confirm = vi.fn(async () => undefined)
    session.registerSessionConfirmation(confirm)
    const api = client.createLabWeaverApiClient({ baseUrl: '/', authentication: { mode: 'bff' } })
    const response = deferred()
    api.instance.defaults.adapter = vi.fn(async (config) => {
      await response.promise
      const body = { data: { diagnosticCode: 'LW_AUTH_SESSION_REJECTED' }, status: 401, statusText: 'Unauthorized', headers: new AxiosHeaders(), config }
      throw new AxiosError('Request failed with status code 401', AxiosError.ERR_BAD_REQUEST, config, null, body as never)
    })

    const listed = api.get({ url: '/api/v1/projects' })
    await new Promise((resolve) => setTimeout(resolve, 0))
    session.advanceSessionEpoch()
    response.resolve()

    expect(await listed).toMatchObject({ diagnosticCode: 'LW_SDK_SESSION_CHANGED' })
    expect(confirm).not.toHaveBeenCalled()
  })

  it('re-reads the actor once for concurrent 401 answers of the current session', async () => {
    const { session, client } = await freshTransport()
    const reread = deferred()
    const confirm = vi.fn(() => reread.promise)
    session.registerSessionConfirmation(confirm)
    const api = client.createLabWeaverApiClient({ baseUrl: '/', authentication: { mode: 'bff' } })
    api.instance.defaults.adapter = vi.fn(async (config) => {
      const body = { data: { diagnosticCode: 'LW_AUTH_SESSION_REJECTED' }, status: 401, statusText: 'Unauthorized', headers: new AxiosHeaders(), config }
      throw new AxiosError('Request failed with status code 401', AxiosError.ERR_BAD_REQUEST, config, null, body as never)
    })

    await Promise.all([api.get({ url: '/api/v1/projects' }), api.get({ url: '/api/v1/works' })])
    reread.resolve()

    expect(confirm).toHaveBeenCalledOnce()
  })

  describe.each([
    { rejection: '401', status: 401, data: { diagnosticCode: 'LW_AUTH_SESSION_REJECTED' } },
    { rejection: 'CSRF 403', status: 403, data: csrfRejected },
  ])('a $rejection reported after the auth interceptor finished and before the adapter runs', ({ status, data }) => {
    /**
     * Write W of actor A has its token; a concurrent response is rejected in
     * the last window of the real Axios chain: `dispatchRequest` runs the
     * request transforms right before it calls the adapter. No client check
     * can follow that point; W may only ever carry A's own token, which the
     * BFF refuses under B's cookie.
     */
    async function writeWithRejectionBeforeAdapter(confirm: (recordSessionActor: (actorId: string) => void) => Promise<void>) {
      const { session, client, axios: transportAxios } = await freshTransport()
      session.recordSessionActor('actor-a')
      vi.spyOn(transportAxios, 'get').mockResolvedValue(csrfFor('actor-a', 'token-of-a'))
      session.registerSessionConfirmation(() => confirm(session.recordSessionActor))
      const api = client.createLabWeaverApiClient({ baseUrl: '/', authentication: { mode: 'bff' } })
      const writes: unknown[] = []
      api.instance.defaults.adapter = vi.fn(async (config) => {
        writes.push(config.headers['X-CSRF-Token'])
        // The BFF binds the token to A's session; under B's cookie it refuses the write.
        const response = { data: csrfRejected, status: 403, statusText: 'Forbidden', headers: new AxiosHeaders(), config }
        throw new AxiosError('Request failed with status code 403', AxiosError.ERR_BAD_REQUEST, config, null, response as never)
      })
      // Axios keeps registered interceptors in `handlers`, which its types do not expose.
      const responseInterceptors = api.instance.interceptors.response as unknown as {
        handlers: { rejected: (error: unknown) => Promise<unknown> }[]
      }
      const transforms = [api.instance.defaults.transformRequest ?? []].flat()
      let rejectionReported = false
      api.instance.defaults.transformRequest = [
        (body: unknown) => {
          if (!rejectionReported) {
            rejectionReported = true
            const response = { data, status, statusText: 'Rejected', headers: new AxiosHeaders(), config: {} }
            const rejected = new AxiosError('Request failed', AxiosError.ERR_BAD_REQUEST, undefined, null, response as never)
            void responseInterceptors.handlers[0].rejected(rejected).catch(() => undefined)
          }
          return body
        },
        ...transforms,
      ]

      const result = await api.post({ url: '/api/v1/users/me/ssh-keys', body: { publicKey: 'A 的公钥' } })
      expect(rejectionReported).toBe(true)
      return { result, writes }
    }

    it('sends at most A\'s own token while the identity re-read is still pending', async () => {
      const reread = deferred()
      const confirm = vi.fn(() => reread.promise)
      const { result, writes } = await writeWithRejectionBeforeAdapter(confirm)

      expect(confirm).toHaveBeenCalledOnce()
      expect(writes).toEqual(['token-of-a'])
      expect(result.error).toMatchObject({ diagnosticCode: 'LW_AUTH_CSRF_REJECTED' })
      reread.resolve()
    })

    it('sends at most A\'s own token and discards the answer once the re-read finds actor B', async () => {
      const { result, writes } = await writeWithRejectionBeforeAdapter(async (recordSessionActor) => {
        recordSessionActor('actor-b')
      })

      expect(writes).toEqual(['token-of-a'])
      expect(result).toMatchObject({ diagnosticCode: 'LW_SDK_SESSION_CHANGED' })
      expect(result.data).toBeUndefined()
    })
  })
})

describe('generated SSE authentication (#198)', () => {
  const eventsUrl = '/api/v1/projects/01900000-0000-7000-8000-000000000010/events'
  const security = [
    { key: 'oidc', scheme: 'bearer', type: 'http' },
    { in: 'cookie', name: '__Host-labweaver_session', type: 'apiKey' },
  ] as const

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('rejects an independent client stream before any request when no bearer is available', async () => {
    const fetch = vi.fn()
    vi.stubGlobal('fetch', fetch)
    const client = makeClient('https://api.example.test', { mode: 'bearer', accessToken: async () => undefined })

    await expect(client.sse.get({ url: eventsUrl, security: [...security] })).rejects.toMatchObject({
      name: 'LabWeaverApiError',
      diagnosticCode: 'LW_SDK_AUTH_TOKEN_UNAVAILABLE',
    })
    expect(fetch).not.toHaveBeenCalled()
  })

  it('sends the bearer only to the HTTP bearer scheme, never as a handwritten session cookie', async () => {
    const fetch = vi.fn(async (_request: Request) => new Response('data: {"streamSequence":"1"}\n\n', {
      status: 200,
      headers: { 'content-type': 'text/event-stream' },
    }))
    vi.stubGlobal('fetch', fetch)
    const accessToken = vi.fn(async () => 'stream-bearer')
    const client = makeClient('https://api.example.test', { mode: 'bearer', accessToken })

    const events = await client.sse.get({ url: eventsUrl, security: [...security] })
    const first = await events.stream.next()

    expect(first.value).toEqual({ streamSequence: '1' })
    expect(accessToken).toHaveBeenCalledOnce()
    const request = fetch.mock.calls[0][0]
    expect(request.headers.get('authorization')).toBe('Bearer stream-bearer')
    expect(request.headers.get('cookie')).toBeNull()
  })

  it('does not give the bearer to a cookie scheme on REST calls either', async () => {
    const client = makeClient('/', { mode: 'bearer', accessToken: async () => 'rest-bearer' })
    const adapter = installSuccessfulAdapter(client)

    await client.get({ url: '/api/v1/platform/configuration', security: [...security] })

    const headers = adapter.mock.calls[0][0].headers
    expect(headers.Authorization).toBe('Bearer rest-bearer')
    expect(headers.Cookie).toBeUndefined()
  })

  it('rejects a default SDK client stream before any request when the OIDC session has no bearer', async () => {
    const { client } = await freshTransport()
    const { getOidcAccessToken } = await import('@/composables/useAuth')
    vi.mocked(getOidcAccessToken).mockResolvedValueOnce(undefined)
    const { client: defaultClient } = await import('@/generated/contracts/client.gen')
    const fetch = vi.fn()
    vi.stubGlobal('fetch', fetch)
    await client.initializeSdkClient()

    await expect(defaultClient.sse.get({ url: eventsUrl, security: [...security] })).rejects.toMatchObject({
      diagnosticCode: 'LW_SDK_AUTH_TOKEN_UNAVAILABLE',
    })
    expect(fetch).not.toHaveBeenCalled()
  })

  describe('streams belong to the session epoch they were opened in', () => {
    const eventStream = () => new Response('data: {"streamSequence":"1"}\n\n', {
      status: 200,
      headers: { 'content-type': 'text/event-stream' },
    })

    it('does not open a stream when the epoch ends while its bearer is read', async () => {
      const { session, client } = await freshTransport()
      const fetch = vi.fn(async (_request: Request) => eventStream())
      vi.stubGlobal('fetch', fetch)
      let answerToken!: (token: string) => void
      const api = client.createLabWeaverApiClient({
        baseUrl: 'https://api.example.test',
        authentication: { mode: 'bearer', accessToken: () => new Promise((resolve) => { answerToken = resolve }) },
      })

      const opening = api.sse.get({ url: eventsUrl, security: [...security] })
      await new Promise((resolve) => setTimeout(resolve, 0))
      // getOidcAccessToken advances the epoch when the token it reads belongs to another actor.
      session.advanceSessionEpoch()
      answerToken('bearer-of-actor-b')

      await expect(opening).rejects.toMatchObject({ diagnosticCode: 'LW_SDK_SESSION_CHANGED' })
      expect(fetch).not.toHaveBeenCalled()
    })

    it('does not fetch on a first next() made after the epoch ended', async () => {
      const { session, client } = await freshTransport()
      const fetch = vi.fn(async (_request: Request) => eventStream())
      vi.stubGlobal('fetch', fetch)
      const api = client.createLabWeaverApiClient({
        baseUrl: 'https://api.example.test',
        authentication: { mode: 'bearer', accessToken: async () => 'bearer-of-actor-a' },
      })

      const events = await api.sse.get({ url: eventsUrl, security: [...security] })
      session.advanceSessionEpoch()

      expect(await events.stream.next()).toEqual({ done: true, value: undefined })
      expect(fetch).not.toHaveBeenCalled()
    })

    it.each([
      { mode: 'bearer' as const },
      { mode: 'bff' as const },
    ])('stops instead of reconnecting once the epoch ends before a retry ($mode)', async ({ mode }) => {
      const { session, client } = await freshTransport()
      const fetch = vi.fn(async (_request: Request) => new Response('', { status: 503 }))
      vi.stubGlobal('fetch', fetch)
      const accessToken = vi.fn(async () => 'bearer-of-actor-a')
      const api = client.createLabWeaverApiClient({
        baseUrl: 'https://api.example.test',
        authentication: mode === 'bearer' ? { mode, accessToken } : { mode },
      })
      const sleeps: number[] = []

      const events = await api.sse.get({
        url: eventsUrl,
        security: [...security],
        // The generated client accepts its backoff hook through the request options.
        ...{
          sseSleepFn: async (ms: number) => {
            sleeps.push(ms)
            // Another actor signs in during the backoff before the reconnect.
            session.advanceSessionEpoch()
          },
        },
      })

      expect(await events.stream.next()).toEqual({ done: true, value: undefined })
      expect(fetch).toHaveBeenCalledOnce()
      expect(sleeps).toHaveLength(1)
      if (mode === 'bearer') expect(accessToken).toHaveBeenCalledOnce()
    })

    it('reads a current bearer before each reconnect in the same epoch', async () => {
      const { client } = await freshTransport()
      const fetch = vi.fn()
        .mockResolvedValueOnce(new Response('', { status: 503 }))
        .mockResolvedValueOnce(eventStream())
      vi.stubGlobal('fetch', fetch)
      const accessToken = vi.fn()
        .mockResolvedValueOnce('bearer-before-refresh')
        .mockResolvedValueOnce('bearer-after-refresh')
      const api = client.createLabWeaverApiClient({
        baseUrl: 'https://api.example.test',
        authentication: { mode: 'bearer', accessToken },
      })

      const events = await api.sse.get({ url: eventsUrl, security: [...security], ...{ sseSleepFn: async () => undefined } })

      expect((await events.stream.next()).value).toEqual({ streamSequence: '1' })
      const authorizations = fetch.mock.calls.map(([request]) => (request as Request).headers.get('authorization'))
      expect(authorizations).toEqual(['Bearer bearer-before-refresh', 'Bearer bearer-after-refresh'])
    })

    it('delivers and reports no buffered event once the epoch ends between two events of one chunk', async () => {
      const { session, client } = await freshTransport()
      vi.stubGlobal('fetch', vi.fn(async (_request: Request) => new Response(
        'data: {"streamSequence":"1"}\n\ndata: {"streamSequence":"2"}\n\n',
        { status: 200, headers: { 'content-type': 'text/event-stream' } },
      )))
      const api = client.createLabWeaverApiClient({
        baseUrl: 'https://api.example.test',
        authentication: { mode: 'bearer', accessToken: async () => 'bearer-of-actor-a' },
      })
      const onSseEvent = vi.fn()

      const events = await api.sse.get({ url: eventsUrl, security: [...security], onSseEvent })
      expect((await events.stream.next()).value).toEqual({ streamSequence: '1' })
      // Another actor signs in while the second event of the same chunk is still buffered.
      session.advanceSessionEpoch()

      expect(await events.stream.next()).toEqual({ done: true, value: undefined })
      expect(onSseEvent).toHaveBeenCalledOnce()
      expect(onSseEvent.mock.calls[0][0].data).toEqual({ streamSequence: '1' })
    })

    /** Abort listeners currently registered on `signal`, observed through its own add/remove calls. */
    function abortListenersOf(signal: AbortSignal) {
      const listeners = new Set<unknown>()
      const add = signal.addEventListener.bind(signal)
      const remove = signal.removeEventListener.bind(signal)
      vi.spyOn(signal, 'addEventListener').mockImplementation((type: string, listener: EventListenerOrEventListenerObject | null, options?: boolean | AddEventListenerOptions) => {
        if (type === 'abort') listeners.add(listener)
        add(type, listener, options)
      })
      vi.spyOn(signal, 'removeEventListener').mockImplementation((type: string, listener: EventListenerOrEventListenerObject | null, options?: boolean | EventListenerOptions) => {
        if (type === 'abort') listeners.delete(listener)
        remove(type, listener, options)
      })
      return listeners
    }

    type Stream = { stream: AsyncGenerator<unknown, void, unknown> }

    it.each([
      {
        ending: 'normal completion',
        responses: () => [eventStream()],
        consume: async ({ stream }: Stream) => {
          expect((await stream.next()).value).toEqual({ streamSequence: '1' })
          expect((await stream.next()).done).toBe(true)
        },
      },
      {
        ending: 'several reconnects with the built-in backoff',
        responses: () => [new Response('', { status: 503 }), new Response('', { status: 503 }), eventStream()],
        consume: async ({ stream }: Stream) => {
          expect((await stream.next()).value).toEqual({ streamSequence: '1' })
          expect((await stream.next()).done).toBe(true)
        },
      },
      {
        ending: 'caller cancellation',
        responses: () => [eventStream()],
        consume: async ({ stream }: Stream, cancel: () => void) => {
          expect((await stream.next()).value).toEqual({ streamSequence: '1' })
          cancel()
          expect((await stream.next()).done).toBe(true)
        },
      },
      {
        ending: 'return() before the first event',
        responses: () => [eventStream()],
        consume: async ({ stream }: Stream) => {
          expect((await stream.return(undefined)).done).toBe(true)
        },
      },
    ])('leaves no listener on the epoch or caller signal after $ending', async ({ responses, consume }) => {
      const { session, client } = await freshTransport()
      const queued = responses()
      const fetch = vi.fn(async (_request: Request) => queued.shift() ?? new Response('', { status: 503 }))
      vi.stubGlobal('fetch', fetch)
      const api = client.createLabWeaverApiClient({
        baseUrl: 'https://api.example.test',
        authentication: { mode: 'bearer', accessToken: async () => 'bearer-of-actor-a' },
      })
      const epochListeners = abortListenersOf(session.sessionEpochSignal())
      const caller = new AbortController()
      const callerListeners = abortListenersOf(caller.signal)

      const events = await api.sse.get({ url: eventsUrl, security: [...security], signal: caller.signal, sseDefaultRetryDelay: 1 })
      expect(epochListeners.size).toBe(1)
      expect(callerListeners.size).toBe(1)
      await consume(events, () => caller.abort())

      expect(epochListeners.size).toBe(0)
      expect(callerListeners.size).toBe(0)
    })
  })
})
