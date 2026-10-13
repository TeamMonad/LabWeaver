import axios from 'axios'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createLabWeaverApiClient } from '@/api/client'

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

function csrfProblem(diagnosticCode: string, status = 403) {
  return {
    type: 'about:blank',
    title: 'Forbidden',
    status,
    detail: 'CSRF token is not valid for the current session.',
    instance: '/api/v1/resource-requests',
    diagnosticCode,
    requestId: 'req-csrf-test',
    retryable: false,
  }
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


  it('rejects SSE auth before fetch when bearer token is unavailable', async () => {
    const fetchSpy = vi.fn()
    vi.stubGlobal('fetch', fetchSpy)
    const client = makeClient('/', { mode: 'bearer', accessToken: async () => undefined })

    await expect(
      client.sse.get({
        url: '/api/v1/projects/01900000-0000-7000-8000-000000000010/events',
        security: [{ scheme: 'bearer', type: 'http' }],
      }),
    ).rejects.toMatchObject({
      name: 'LabWeaverApiError',
      diagnosticCode: 'LW_SDK_AUTH_TOKEN_UNAVAILABLE',
    })
    expect(fetchSpy).not.toHaveBeenCalled()
  })

  it('auth resolver returns undefined for cookie apiKey security (no handwritten Cookie header)', async () => {
    const accessToken = vi.fn(async () => 'should-not-be-used-for-cookie')
    const client = makeClient('/', { mode: 'bearer', accessToken })

    const auth = client.getConfig().auth
    expect(auth).toBeTypeOf('function')
    await expect(
      auth!({ type: 'apiKey', in: 'cookie', name: '__Host-labweaver_session' }),
    ).resolves.toBeUndefined()
    expect(accessToken).not.toHaveBeenCalled()

    const adapter = installSuccessfulAdapter(client)
    await client.get({
      url: '/api/v1/platform/configuration',
      security: [{ type: 'apiKey', in: 'cookie', name: '__Host-labweaver_session' }],
    })

    expect(adapter).toHaveBeenCalledOnce()
    const requestConfig = adapter.mock.calls[0][0]
    expect(requestConfig.headers.Cookie).toBeUndefined()
    expect(requestConfig.headers.cookie).toBeUndefined()
  })

  it('auth resolver still returns raw token for HTTP bearer schemes', async () => {
    const accessToken = vi.fn(async () => 'sse-bearer-token')
    const client = makeClient('/', { mode: 'bearer', accessToken })
    const auth = client.getConfig().auth
    expect(auth).toBeTypeOf('function')
    await expect(auth!({ type: 'http', scheme: 'bearer' })).resolves.toBe('sse-bearer-token')
    expect(accessToken).toHaveBeenCalledOnce()
  })


  it('caches BFF CSRF until near expiry on the happy path', async () => {
    const csrfGet = vi.spyOn(axios, 'get').mockResolvedValue({
      data: {
        csrfToken: 'csrf-cached',
        expiresAt: new Date(Date.now() + 120_000).toISOString(),
      },
    } as never)
    const client = makeClient('/', { mode: 'bff' })
    const adapter = installSuccessfulAdapter(client)

    await client.post({ url: '/api/v1/resource-requests', body: { n: 1 } })
    await client.post({ url: '/api/v1/resource-requests', body: { n: 2 } })

    expect(csrfGet).toHaveBeenCalledTimes(1)
    expect(adapter).toHaveBeenCalledTimes(2)
    expect(adapter.mock.calls[0][0].headers['X-CSRF-Token']).toBe('csrf-cached')
    expect(adapter.mock.calls[1][0].headers['X-CSRF-Token']).toBe('csrf-cached')
  })

  it('after cross-tab session replacement, invalidates CSRF cache on LW_AUTH_CSRF_REJECTED without replaying the write, then recovers', async () => {
    let sessionToken = 'csrf-session-a'
    const csrfGet = vi.spyOn(axios, 'get').mockImplementation(async () => ({
      data: {
        csrfToken: sessionToken,
        expiresAt: new Date(Date.now() + 120_000).toISOString(),
      },
    }))

    const client = makeClient('/', { mode: 'bff' })
    const adapter = vi.fn(async (config) => {
      const sent = config.headers['X-CSRF-Token']
      // Simulate Access rejecting a token that belongs to the previous session.
      if (sent === 'csrf-session-a' && sessionToken === 'csrf-session-b') {
        const error = new axios.AxiosError(
          'Request failed with status code 403',
          'ERR_BAD_REQUEST',
          config,
          null,
          {
            status: 403,
            statusText: 'Forbidden',
            headers: {},
            config,
            data: csrfProblem('LW_AUTH_CSRF_REJECTED'),
          },
        )
        throw error
      }
      return {
        data: { ok: true, actor: sessionToken },
        status: 200,
        statusText: 'OK',
        headers: {},
        config,
      }
    })
    client.instance.defaults.adapter = adapter

    // Tab A caches csrf-A and completes a write as user A.
    await client.post({ url: '/api/v1/resource-requests', body: { as: 'user-a' } })
    expect(csrfGet).toHaveBeenCalledTimes(1)
    expect(adapter.mock.calls[0][0].headers['X-CSRF-Token']).toBe('csrf-session-a')

    // Other tab logout/login switches the shared cookie to session B.
    sessionToken = 'csrf-session-b'

    // Stale-cache write fails; must not auto-replay the non-idempotent body.
    await expect(
      client.post({
        url: '/api/v1/resource-requests',
        body: { as: 'user-a-ui-stale' },
        throwOnError: true,
      }),
    ).rejects.toMatchObject({
      response: {
        status: 403,
        data: { diagnosticCode: 'LW_AUTH_CSRF_REJECTED' },
      },
    })
    expect(adapter).toHaveBeenCalledTimes(2)
    expect(adapter.mock.calls[1][0].headers['X-CSRF-Token']).toBe('csrf-session-a')
    expect(JSON.parse(adapter.mock.calls[1][0].data as string)).toEqual({ as: 'user-a-ui-stale' })
    // No silent retry: still a single CSRF fetch from before the switch.
    expect(csrfGet).toHaveBeenCalledTimes(1)

    // Next write refetches session-B token and succeeds (no old-user silent submit).
    await client.post({ url: '/api/v1/resource-requests', body: { as: 'user-b' } })
    expect(csrfGet).toHaveBeenCalledTimes(2)
    expect(adapter).toHaveBeenCalledTimes(3)
    expect(adapter.mock.calls[2][0].headers['X-CSRF-Token']).toBe('csrf-session-b')
    expect(JSON.parse(adapter.mock.calls[2][0].data as string)).toEqual({ as: 'user-b' })
  })

  it('does not invalidate BFF CSRF cache on unrelated 403 diagnostics', async () => {
    const csrfGet = vi.spyOn(axios, 'get').mockResolvedValue({
      data: {
        csrfToken: 'csrf-keep',
        expiresAt: new Date(Date.now() + 120_000).toISOString(),
      },
    } as never)
    const client = makeClient('/', { mode: 'bff' })
    let calls = 0
    const adapter = vi.fn(async (config) => {
      calls += 1
      if (calls === 1) {
        const error = new axios.AxiosError(
          'Request failed with status code 403',
          'ERR_BAD_REQUEST',
          config,
          null,
          {
            status: 403,
            statusText: 'Forbidden',
            headers: {},
            config,
            data: csrfProblem('LW_AUTHZ_DENIED'),
          },
        )
        throw error
      }
      return {
        data: { ok: true },
        status: 200,
        statusText: 'OK',
        headers: {},
        config,
      }
    })
    client.instance.defaults.adapter = adapter

    await expect(
      client.post({ url: '/api/v1/resource-requests', body: {}, throwOnError: true }),
    ).rejects.toMatchObject({
      response: { status: 403, data: { diagnosticCode: 'LW_AUTHZ_DENIED' } },
    })
    await client.post({ url: '/api/v1/resource-requests', body: {} })

    expect(csrfGet).toHaveBeenCalledTimes(1)
    expect(adapter.mock.calls[1][0].headers['X-CSRF-Token']).toBe('csrf-keep')
  })

  it('rejects a contract-prefixed API base URL', () => {
    expect(() => makeClient('/api/v1', { mode: 'bearer', accessToken: async () => 'token' })).toThrowError(
      'Public API base URL must identify the origin, not repeat the /api/v1 contract prefix.',
    )
  })
})
