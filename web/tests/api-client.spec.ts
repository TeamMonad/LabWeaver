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

  it('does not replay a CSRF-rejected write, re-reads the actor first and lets the user resubmit with a new token', async () => {
    const { session, client, axios: transportAxios } = await freshTransport()
    const csrfGet = vi.spyOn(transportAxios, 'get')
      .mockResolvedValueOnce({ data: { csrfToken: 'token-old-session', expiresAt: future() } } as never)
      .mockResolvedValueOnce({ data: { csrfToken: 'token-current-session', expiresAt: future() } } as never)
    const reread = deferred()
    const confirm = vi.fn(() => reread.promise)
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

    // The user submits again while the actor is still being read: nothing is sent yet.
    const resubmitted = api.post({ url: '/api/v1/projects', body: { name: '手动重新提交' } })
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(sent).toHaveLength(1)

    reread.resolve()
    expect((await resubmitted).error).toBeUndefined()
    expect(sent).toEqual([
      { token: 'token-old-session', body: { name: '第一次提交' } },
      { token: 'token-current-session', body: { name: '手动重新提交' } },
    ])
    expect(csrfGet).toHaveBeenCalledTimes(2)
  })

  it('never sends a waiting write once the re-read finds another actor', async () => {
    const { session, client, axios: transportAxios } = await freshTransport()
    vi.spyOn(transportAxios, 'get').mockResolvedValue({ data: { csrfToken: 'token', expiresAt: future() } } as never)
    const reread = deferred()
    session.registerSessionConfirmation(async () => {
      await reread.promise
      // useAuth found actor B and advanced the epoch.
      session.advanceSessionEpoch()
    })
    const api = client.createLabWeaverApiClient({ baseUrl: '/', authentication: { mode: 'bff' } })
    const adapter = installSuccessfulAdapter(api)

    session.reportSessionRejected()
    const staleForm = api.post({ url: '/api/v1/users/me/ssh-keys', body: { publicKey: 'A 的公钥' } })
    await new Promise((resolve) => setTimeout(resolve, 0))
    reread.resolve()

    const result = await staleForm
    expect(result).toMatchObject({ diagnosticCode: 'LW_SDK_SESSION_CHANGED' })
    expect(result.data).toBeUndefined()
    expect(adapter).not.toHaveBeenCalled()
  })

  it('does not send while the actor cannot be read and reads it again on the next request', async () => {
    const { session, client } = await freshTransport()
    let failRead!: (error: Error) => void
    const confirm = vi.fn()
      .mockReturnValueOnce(new Promise<void>((_, fail) => { failRead = fail }))
      .mockResolvedValueOnce(undefined)
    session.registerSessionConfirmation(confirm)
    const api = client.createLabWeaverApiClient({ baseUrl: '/', authentication: { mode: 'bearer', accessToken: async () => 'token' } })
    const adapter = installSuccessfulAdapter(api)

    session.reportSessionRejected()
    const waiting = api.get({ url: '/api/v1/projects' })
    await new Promise((resolve) => setTimeout(resolve, 0))
    failRead(new Error('BFF session lookup failed with HTTP 503'))
    expect(await waiting).toMatchObject({ diagnosticCode: 'LW_SDK_SESSION_UNCONFIRMED' })
    expect(adapter).not.toHaveBeenCalled()

    expect((await api.get({ url: '/api/v1/projects' })).error).toBeUndefined()
    expect(confirm).toHaveBeenCalledTimes(2)
    expect(adapter).toHaveBeenCalledOnce()
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

  it('re-reads the actor when the BFF answers 401 for the current session', async () => {
    const { session, client } = await freshTransport()
    const confirm = vi.fn(async () => undefined)
    session.registerSessionConfirmation(confirm)
    const api = client.createLabWeaverApiClient({ baseUrl: '/', authentication: { mode: 'bff' } })
    api.instance.defaults.adapter = vi.fn(async (config) => {
      const body = { data: { diagnosticCode: 'LW_AUTH_SESSION_REJECTED' }, status: 401, statusText: 'Unauthorized', headers: new AxiosHeaders(), config }
      throw new AxiosError('Request failed with status code 401', AxiosError.ERR_BAD_REQUEST, config, null, body as never)
    })

    await api.get({ url: '/api/v1/projects' })

    expect(confirm).toHaveBeenCalledOnce()
  })

  describe('a rejection that arrives while a write is fetching its CSRF token', () => {
    /**
     * Write W passes the session check, then waits for its CSRF token. While it
     * waits, a concurrent GET on the same client receives 401, which starts a
     * re-read of the actor. Only then does W's token arrive.
     */
    async function writeWaitingForCsrf(confirm: (advanceSessionEpoch: () => void) => Promise<void>) {
      const { session, client, axios: transportAxios } = await freshTransport()
      let answerCsrf!: (value: unknown) => void
      const csrfGet = vi.spyOn(transportAxios, 'get')
        .mockReturnValueOnce(new Promise((resolve) => { answerCsrf = resolve }) as never)
        .mockResolvedValueOnce({ data: { csrfToken: 'token-after-reread', expiresAt: future() } } as never)
      session.registerSessionConfirmation(() => confirm(session.advanceSessionEpoch))
      const api = client.createLabWeaverApiClient({ baseUrl: '/', authentication: { mode: 'bff' } })
      const writes: unknown[] = []
      api.instance.defaults.adapter = vi.fn(async (config) => {
        if (config.method === 'get') {
          const body = { data: { diagnosticCode: 'LW_AUTH_SESSION_REJECTED' }, status: 401, statusText: 'Unauthorized', headers: new AxiosHeaders(), config }
          throw new AxiosError('Request failed with status code 401', AxiosError.ERR_BAD_REQUEST, config, null, body as never)
        }
        writes.push(config.headers['X-CSRF-Token'])
        return { data: { ok: true }, status: 201, statusText: 'Created', headers: {}, config }
      })

      const write = api.post({ url: '/api/v1/users/me/ssh-keys', body: { publicKey: 'A 的公钥' } })
      await vi.waitFor(() => expect(csrfGet).toHaveBeenCalledOnce())
      await api.get({ url: '/api/v1/projects' })
      answerCsrf({ data: { csrfToken: 'token-before-reread', expiresAt: future() } })
      await new Promise((resolve) => setTimeout(resolve, 0))
      return { write, writes }
    }

    it('waits for the re-read that is still in flight before sending', async () => {
      const reread = deferred()
      const confirm = vi.fn(() => reread.promise)
      const { write, writes } = await writeWaitingForCsrf(confirm)

      expect(confirm).toHaveBeenCalledOnce()
      expect(writes).toEqual([])

      // Same actor: the write was never sent, so sending it now with a token for the confirmed session is not a replay.
      reread.resolve()
      expect((await write).error).toBeUndefined()
      expect(writes).toEqual(['token-after-reread'])
    })

    it('does not send when the re-read fails', async () => {
      let failRead!: (error: Error) => void
      const { write, writes } = await writeWaitingForCsrf(() => new Promise<void>((_, fail) => { failRead = fail }))

      failRead(new Error('BFF session lookup failed with HTTP 503'))
      const result = await write

      expect(result).toMatchObject({ diagnosticCode: 'LW_SDK_SESSION_UNCONFIRMED' })
      expect(result.data).toBeUndefined()
      expect(writes).toEqual([])
    })

    it('does not send when the re-read finds another actor', async () => {
      const reread = deferred()
      const { write, writes } = await writeWaitingForCsrf(async (advanceSessionEpoch) => {
        await reread.promise
        // useAuth advances the epoch when the re-read returns actor B.
        advanceSessionEpoch()
      })

      reread.resolve()
      const result = await write

      expect(result).toMatchObject({ diagnosticCode: 'LW_SDK_SESSION_CHANGED' })
      expect(result.data).toBeUndefined()
      expect(writes).toEqual([])
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
})
