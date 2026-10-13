import axios, { AxiosError, type AxiosInstance } from 'axios'
import { API_AUTH_MODE, API_BASE_URL } from '@/config'
import { getOidcAccessToken } from '@/composables/useAuth'
import {
  isBffWriteBlocked,
  notifyBffCsrfSessionInvalidation,
} from '@/api/bffSessionBarrier'
import type { ProblemDetails } from '@/generated/contracts'
import type { Auth } from '@/generated/contracts/core/auth.gen'
import { createClient, type Client } from '@/generated/contracts/client'
import { client as defaultSdkClient } from '@/generated/contracts/client.gen'

export type AccessTokenProvider = () => Promise<string | undefined>
export type CsrfTokenProvider = () => Promise<string | undefined>

export type LabWeaverApiAuthentication =
  | { mode: 'bearer'; accessToken: AccessTokenProvider }
  | { mode: 'bff'; csrfToken?: CsrfTokenProvider }

export interface LabWeaverApiClientOptions {
  baseUrl: string
  authentication: LabWeaverApiAuthentication
  timeoutMilliseconds?: number
}

export type LabWeaverClientDiagnostic =
  | 'LW_SDK_CONFIGURATION_INVALID'
  | 'LW_SDK_AUTH_TOKEN_UNAVAILABLE'
  | 'LW_SDK_SESSION_REFRESH_REQUIRED'
  | 'LW_SDK_PROBLEM_INVALID'
  | 'LW_SDK_REQUEST_CANCELLED'
  | 'LW_SDK_REQUEST_TIMEOUT'
  | 'LW_SDK_TRANSPORT_FAILED'

export class LabWeaverApiError extends Error {
  readonly diagnosticCode: LabWeaverClientDiagnostic | string
  readonly problem?: ProblemDetails

  constructor(diagnosticCode: LabWeaverClientDiagnostic | string, message: string, problem?: ProblemDetails) {
    super(message)
    this.name = 'LabWeaverApiError'
    this.diagnosticCode = diagnosticCode
    this.problem = problem
  }
}

function normalizedBaseUrl(value: string): string {
  const raw = value.trim()
  // The generated client appends its path to this value. An empty base keeps
  // root-origin requests single-slash relative instead of producing a
  // protocol-relative request such as //api/....
  if (raw === '/') return ''
  const trimmed = raw.replace(/\/+$/, '')
  if (!trimmed || trimmed.endsWith('/api/v1')) {
    throw new LabWeaverApiError(
      'LW_SDK_CONFIGURATION_INVALID',
      'Public API base URL must identify the origin, not repeat the /api/v1 contract prefix.',
    )
  }
  return trimmed
}

function isProblemDetails(value: unknown): value is ProblemDetails {
  if (!value || typeof value !== 'object') return false
  const problem = value as Record<string, unknown>
  return (
    typeof problem.type === 'string' &&
    typeof problem.title === 'string' &&
    typeof problem.status === 'number' &&
    problem.status >= 400 &&
    problem.status <= 599 &&
    typeof problem.detail === 'string' &&
    typeof problem.instance === 'string' &&
    typeof problem.diagnosticCode === 'string' &&
    /^LW_[A-Z0-9_]+$/.test(problem.diagnosticCode) &&
    typeof problem.requestId === 'string' &&
    typeof problem.retryable === 'boolean'
  )
}

function isUnsafeMethod(method: string | undefined): boolean {
  return !['get', 'head', 'options'].includes((method ?? 'get').toLowerCase())
}

/** Diagnostics that mean the cached synchronizer token is no longer valid for the current session. */
const CSRF_CACHE_INVALIDATING_DIAGNOSTICS = new Set([
  'LW_AUTH_CSRF_REJECTED',
  'LW_AUTH_CSRF_REQUIRED',
])

type BffCsrfTokenCache = {
  getToken: CsrfTokenProvider
  invalidate: () => void
}

/**
 * Caches BFF CSRF tokens by expiry (30s skew) and supports explicit invalidation.
 * Session cookies are HttpOnly, so cross-tab login/logout cannot be keyed from JS;
 * callers invalidate when Access returns a CSRF rejection diagnostic.
 */
export function createBffCsrfTokenProvider(baseUrl: string, timeout: number): BffCsrfTokenCache {
  let cached: { token: string; expiresAt: number } | undefined
  let inFlight: Promise<string | undefined> | undefined
  // Bumps on invalidate so a fetch that started before a session switch cannot
  // repopulate the cache after invalidate() clears it.
  let epoch = 0

  const invalidate = (): void => {
    epoch += 1
    cached = undefined
    inFlight = undefined
  }

  const getToken: CsrfTokenProvider = async () => {
    if (cached && cached.expiresAt - Date.now() > 30_000) return cached.token
    if (inFlight) return inFlight
    const fetchEpoch = epoch
    inFlight = axios
      .get('/api/v1/auth/csrf', {
        baseURL: baseUrl,
        timeout,
        withCredentials: true,
        headers: { Accept: 'application/json, application/problem+json' },
      })
      .then((response) => {
        const body = response.data as Record<string, unknown>
        const expiresAt = typeof body.expiresAt === 'string' ? Date.parse(body.expiresAt) : Number.NaN
        if (typeof body.csrfToken !== 'string' || !body.csrfToken || !Number.isFinite(expiresAt)) {
          throw new LabWeaverApiError('LW_SDK_PROBLEM_INVALID', 'The BFF returned an invalid CSRF response.')
        }
        if (fetchEpoch !== epoch) {
          // Session changed while this fetch was in flight; do not cache a stale result.
          return body.csrfToken
        }
        cached = { token: body.csrfToken, expiresAt }
        return cached.token
      })
      .finally(() => {
        if (fetchEpoch === epoch) inFlight = undefined
      })
    return inFlight
  }

  return { getToken, invalidate }
}

function isCsrfCacheInvalidatingError(error: AxiosError): boolean {
  const status = error.response?.status
  if (status !== 403 && status !== 401) return false
  const data = error.response?.data
  if (!isProblemDetails(data)) return false
  return CSRF_CACHE_INVALIDATING_DIAGNOSTICS.has(data.diagnosticCode)
}

/** Strictly decodes the RFC 9457 extension shared by all generated SDK calls. */
export function decodeProblemDetails(error: unknown): ProblemDetails | undefined {
  if (error instanceof LabWeaverApiError) return error.problem
  if (!axios.isAxiosError(error)) return undefined
  return isProblemDetails(error.response?.data) ? error.response.data : undefined
}

function attachAuthInterceptor(
  instance: AxiosInstance,
  authentication: LabWeaverApiAuthentication,
): { invalidateCsrf?: () => void } {
  const csrf: { getToken: CsrfTokenProvider; invalidate?: () => void } | undefined =
    authentication.mode !== 'bff'
      ? undefined
      : authentication.csrfToken
        ? { getToken: authentication.csrfToken }
        : createBffCsrfTokenProvider(instance.defaults.baseURL ?? '', instance.defaults.timeout as number)

  instance.interceptors.request.use(async (config) => {
    if (authentication.mode === 'bearer') {
      const token = await authentication.accessToken()
      if (!token) {
        throw new LabWeaverApiError('LW_SDK_AUTH_TOKEN_UNAVAILABLE', 'A current OIDC bearer token is required.')
      } else {
        config.headers.Authorization = `Bearer ${token}`
      }
    } else if (isUnsafeMethod(config.method)) {
      // After a CSRF session rejection, block writes until useAuth (or tests)
      // acknowledge a refreshed identity — prevents re-submitting a stale form as B.
      if (isBffWriteBlocked()) {
        throw new LabWeaverApiError(
          'LW_SDK_SESSION_REFRESH_REQUIRED',
          'BFF session changed; refresh the authenticated UI before retrying this write.',
        )
      }
      const token = await csrf?.getToken()
      if (!token) {
        throw new LabWeaverApiError('LW_SDK_AUTH_TOKEN_UNAVAILABLE', 'A current BFF CSRF token is required.')
      }
      config.headers['X-CSRF-Token'] = token
    }
    return config
  })

  return { invalidateCsrf: csrf?.invalidate }
}

function attachResponseInterceptor(
  instance: AxiosInstance,
  options?: { onCsrfRejection?: () => void },
): void {
  instance.interceptors.response.use(
    (response) => response,
    (error: unknown) => {
      if (error instanceof LabWeaverApiError) return Promise.reject(error)
      if (!axios.isAxiosError(error)) {
        return Promise.reject(new LabWeaverApiError('LW_SDK_TRANSPORT_FAILED', 'The API transport failed.'))
      }
      // Invalidate only on known CSRF diagnostics — never auto-replay the failed write.
      if (options?.onCsrfRejection && isCsrfCacheInvalidatingError(error)) {
        options.onCsrfRejection()
      }
      if (axios.isCancel(error) || error.code === AxiosError.ERR_CANCELED) {
        return Promise.reject(new LabWeaverApiError('LW_SDK_REQUEST_CANCELLED', 'The API request was cancelled.'))
      }
      if (error.code === AxiosError.ECONNABORTED) {
        return Promise.reject(new LabWeaverApiError('LW_SDK_REQUEST_TIMEOUT', 'The API request timed out.'))
      }
      // Preserve AxiosError and ProblemDetails so the generated SDK can expose
      // response.data through its result.error field.
      return Promise.reject(error)
    },
  )
}

/**
 * Shared auth resolver for generated REST/SSE.
 * Only HTTP bearer schemes receive an OIDC token; cookie/apiKey schemes return
 * undefined so setAuthParams does not invent a handwritten Cookie header.
 */
function resolveBearerAuth(
  accessToken: AccessTokenProvider,
): (auth: Auth) => Promise<string | undefined> {
  return async (auth) => {
    if (auth.type !== 'http' || auth.scheme !== 'bearer') {
      return undefined
    }
    const token = await accessToken()
    if (!token) {
      throw new LabWeaverApiError('LW_SDK_AUTH_TOKEN_UNAVAILABLE', 'A current OIDC bearer token is required.')
    }
    return token
  }
}

/** The only supported Public SDK initialization path for browser consumers. */
export function createLabWeaverApiClient(options: LabWeaverApiClientOptions): Client {
  const timeout = options.timeoutMilliseconds ?? 30_000
  if (!Number.isSafeInteger(timeout) || timeout <= 0 || timeout > 120_000) {
    throw new LabWeaverApiError('LW_SDK_CONFIGURATION_INVALID', 'API timeout must be between 1 and 120000 ms.')
  }

  const baseUrl = normalizedBaseUrl(options.baseUrl)
  const instance = axios.create({
    baseURL: baseUrl,
    timeout,
    withCredentials: options.authentication.mode === 'bff',
    headers: { Accept: 'application/json, application/problem+json' },
  })

  const { invalidateCsrf } = attachAuthInterceptor(instance, options.authentication)
  attachResponseInterceptor(instance, {
    onCsrfRejection: invalidateCsrf
      ? () => {
          invalidateCsrf()
          notifyBffCsrfSessionInvalidation()
        }
      : undefined,
  })

  // Axios interceptors cover REST; generated SSE uses fetch via createSseClient
  // and only picks up Authorization through the generated `auth` resolver.
  // That resolver must reject like the Axios interceptor — returning undefined
  // lets generated SSE issue an unauthenticated fetch.
  const authentication = options.authentication
  const auth =
    authentication.mode === 'bearer'
      ? resolveBearerAuth(authentication.accessToken)
      : undefined

  return createClient({
    axios: instance,
    baseURL: baseUrl,
    timeout,
    auth,
    // BFF SSE (fetch) must send the session cookie; REST already uses axios withCredentials.
    ...(authentication.mode === 'bff' ? { credentials: 'include' } : {}),
  } as Parameters<typeof createClient>[0])
}

export const apiClient = createLabWeaverApiClient({
  baseUrl: API_BASE_URL,
  authentication:
    API_AUTH_MODE === 'bff'
      ? { mode: 'bff' }
      : { mode: 'bearer', accessToken: getOidcAccessToken },
})

// The generated SDK functions include the full /api/v1 path, so the SDK transport
// uses an empty baseURL. It shares auth/response handling with apiClient but stays
// a separate instance so that direct Public API calls and SDK calls do not collide
// on baseURL normalization.
const sdkTransport: AxiosInstance = axios.create({
  baseURL: '',
  timeout: 30_000,
  withCredentials: API_AUTH_MODE === 'bff',
  headers: { Accept: 'application/json, application/problem+json' },
})

const { invalidateCsrf: invalidateSdkCsrf } = attachAuthInterceptor(
  sdkTransport,
  API_AUTH_MODE === 'bff'
    ? { mode: 'bff' }
    : { mode: 'bearer', accessToken: getOidcAccessToken },
)
attachResponseInterceptor(sdkTransport, {
  onCsrfRejection: invalidateSdkCsrf
    ? () => {
        invalidateSdkCsrf()
        notifyBffCsrfSessionInvalidation()
      }
    : undefined,
})

/** Initialize the generated SDK client transport before the Vue app mounts. */
export async function initializeSdkClient(): Promise<void> {
  // Make the generated SDK functions use the configured SDK transport.
  defaultSdkClient.setConfig({
    axios: sdkTransport,
    baseURL: '',
    auth:
      API_AUTH_MODE === 'bearer'
        ? resolveBearerAuth(getOidcAccessToken)
        : // BFF mode relies on the session cookie; same-origin fetch sends it.
          undefined,
  })
}

/** Health checks are intentionally outside the authenticated Public API contract. */
export async function healthCheck(options?: { timeout?: number }): Promise<{ ok: boolean; status: number }> {
  const controller = new AbortController()
  const timeout = options?.timeout ?? 5000
  const timer = setTimeout(() => controller.abort(), timeout)
  try {
    const response = await fetch('/health/live', { signal: controller.signal, cache: 'no-store' })
    return { ok: response.ok, status: response.status }
  } finally {
    clearTimeout(timer)
  }
}
