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
