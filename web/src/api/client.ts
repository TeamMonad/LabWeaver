import axios, { AxiosError, type AxiosInstance } from 'axios'
import { API_AUTH_MODE, API_BASE_URL } from '@/config'
import { getOidcAccessToken } from '@/composables/useAuth'
import type { ProblemDetails } from '@/generated/contracts'
import { createClient, type Client } from '@/generated/contracts/client'
import { client as defaultSdkClient } from '@/generated/contracts/client.gen'
import type { Auth } from '@/generated/contracts/core/auth.gen'
import { REQUEST_ID_HEADER, rememberHeaderRequestId } from './requestId'
import { awaitConfirmedSession, reportSessionRejected, sessionCredentialVersion, sessionEpoch } from './session'

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
  | 'LW_SDK_PROBLEM_INVALID'
  | 'LW_SDK_REQUEST_CANCELLED'
  | 'LW_SDK_REQUEST_TIMEOUT'
  | 'LW_SDK_TRANSPORT_FAILED'
  | 'LW_SDK_SESSION_CHANGED'
  | 'LW_SDK_SESSION_UNCONFIRMED'

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

function createBffCsrfTokenProvider(baseUrl: string, timeout: number): CsrfTokenProvider {
  // A token belongs to one session: another actor, a confirmed sign-out or any
  // CSRF/session rejection changes the credential version and retires it.
  let cached: { token: string; expiresAt: number; version: string } | undefined
  let inFlight: { token: Promise<string>; version: string } | undefined
  return async () => {
    const version = sessionCredentialVersion()
    if (cached?.version === version && cached.expiresAt - Date.now() > 30_000) return cached.token
    if (inFlight?.version === version) return inFlight.token
    const token = axios
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
        // A token issued while the session changed is not kept for later writes.
        if (sessionCredentialVersion() === version) cached = { token: body.csrfToken, expiresAt, version }
        return body.csrfToken
      })
      .finally(() => {
        if (inFlight?.token === token) inFlight = undefined
      })
    inFlight = { token, version }
    return token
  }
}

/** The BFF no longer accepts this tab's session cookie or the CSRF token issued for it. */
function isSessionRejection(error: AxiosError): boolean {
  const status = error.response?.status
  if (status === 401) return true
  const problem = error.response?.data
  return status === 403 && isProblemDetails(problem) && problem.diagnosticCode === 'LW_AUTH_CSRF_REJECTED'
}

/** The session epoch each request started in, keyed by its Axios config. */
const requestEpochs = new WeakMap<object, number>()

function startedInEarlierSession(config: object | undefined): boolean {
  const started = config ? requestEpochs.get(config) : undefined
  return started !== undefined && started !== sessionEpoch.value
}

function sessionChanged(): LabWeaverApiError {
  return new LabWeaverApiError(
    'LW_SDK_SESSION_CHANGED',
    'The signed-in session changed; the result of a request started by the previous session was discarded.',
  )
}

/** Strictly decodes the RFC 9457 extension shared by all generated SDK calls. */
export function decodeProblemDetails(error: unknown): ProblemDetails | undefined {
  if (error instanceof LabWeaverApiError) return error.problem
  if (!axios.isAxiosError(error)) return undefined
  return isProblemDetails(error.response?.data) ? error.response.data : undefined
}

function attachAuthInterceptor(instance: AxiosInstance, authentication: LabWeaverApiAuthentication): void {
  const csrfToken =
    authentication.mode === 'bff'
      ? (authentication.csrfToken ?? createBffCsrfTokenProvider(instance.defaults.baseURL ?? '', instance.defaults.timeout as number))
      : undefined

  instance.interceptors.request.use(async (config) => {
    requestEpochs.set(config, sessionEpoch.value)
    // Credentials are fetched asynchronously, and a concurrent request may report
    // a rejection meanwhile: the cookie may then already belong to another actor.
    // The credential version is taken before the gate, so a rejection reported
    // while this request resumes is never mistaken for a confirmed one. Any
    // change sends the request through the gate again with fresh credentials;
    // it has never reached the server, so this is not a replay.
    for (;;) {
      const credentialVersion = sessionCredentialVersion()
      if (!(await awaitConfirmedSession())) {
        throw new LabWeaverApiError(
          'LW_SDK_SESSION_UNCONFIRMED',
          'The signed-in session could not be confirmed; the request was not sent.',
        )
      }
      // The actor changed while this request waited: a form of the previous actor is never sent as the new one.
      if (startedInEarlierSession(config)) throw sessionChanged()
      if (sessionCredentialVersion() !== credentialVersion) continue
      if (authentication.mode === 'bearer') {
        const token = await authentication.accessToken()
        if (!token) {
          throw new LabWeaverApiError('LW_SDK_AUTH_TOKEN_UNAVAILABLE', 'A current OIDC bearer token is required.')
        } else {
          config.headers.Authorization = `Bearer ${token}`
        }
      } else if (isUnsafeMethod(config.method)) {
        const token = await csrfToken?.()
        if (!token) {
          throw new LabWeaverApiError('LW_SDK_AUTH_TOKEN_UNAVAILABLE', 'A current BFF CSRF token is required.')
        }
        config.headers['X-CSRF-Token'] = token
      }
      if (sessionCredentialVersion() === credentialVersion) return config
    }
  })
}

/**
 * `sessionRejected` runs in BFF mode when the server refuses the session or its
 * CSRF token. The failed request is not replayed; its caller shows the error and
 * the user decides whether to submit again once the actor has been read again.
 */
function attachResponseInterceptor(instance: AxiosInstance, sessionRejected?: () => void): void {
  instance.interceptors.response.use(
    (response) => (startedInEarlierSession(response.config) ? Promise.reject(sessionChanged()) : response),
    (error: unknown) => {
      if (error instanceof LabWeaverApiError) return Promise.reject(error)
      if (!axios.isAxiosError(error)) {
        return Promise.reject(new LabWeaverApiError('LW_SDK_TRANSPORT_FAILED', 'The API transport failed.'))
      }
      if (startedInEarlierSession(error.config)) return Promise.reject(sessionChanged())
      if (sessionRejected && isSessionRejection(error)) sessionRejected()
      if (axios.isCancel(error) || error.code === AxiosError.ERR_CANCELED) {
        return Promise.reject(new LabWeaverApiError('LW_SDK_REQUEST_CANCELLED', 'The API request was cancelled.'))
      }
      if (error.code === AxiosError.ECONNABORTED) {
        return Promise.reject(new LabWeaverApiError('LW_SDK_REQUEST_TIMEOUT', 'The API request timed out.'))
      }
      // Keep the server's x-request-id with the error body (the generated SDK
      // hands that same object back as result.error) and with the error itself.
      const headers = error.response?.headers as { get?: (name: string) => unknown; [name: string]: unknown } | undefined
      const headerValue = typeof headers?.get === 'function' ? headers.get(REQUEST_ID_HEADER) : headers?.[REQUEST_ID_HEADER]
      rememberHeaderRequestId(error, headerValue)
      rememberHeaderRequestId(error.response?.data, headerValue)
      // Preserve AxiosError and ProblemDetails so the generated SDK can expose
      // response.data through its result.error field.
      return Promise.reject(error)
    },
  )
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

  const { authentication } = options
  attachAuthInterceptor(instance, authentication)
  attachResponseInterceptor(instance, authentication.mode === 'bff' ? reportSessionRejected : undefined)

  // REST receives its bearer from the Axios interceptor; no client-level `auth`
  // is configured, so no generated security scheme receives a credential it
  // does not use (a cookie scheme would otherwise get a handwritten Cookie header).
  const client = createClient({
    axios: instance,
    baseURL: baseUrl,
    timeout,
  })
  if (authentication.mode === 'bearer') requireStreamBearer(client, authentication.accessToken)
  return client
}

type StreamOptions = Parameters<Client['sse']['get']>[0]

/**
 * Generated SSE opens streams with fetch, outside the Axios interceptors that
 * fail closed for REST. Resolve the bearer once before opening and reject
 * without sending any request when there is none.
 */
function requireStreamBearer(client: Client, accessToken: AccessTokenProvider): void {
  const sse = client.sse as Record<keyof Client['sse'], (options: StreamOptions) => Promise<unknown>>
  for (const method of Object.keys(sse) as (keyof Client['sse'])[]) {
    const open = sse[method]
    sse[method] = async (options) => {
      const token = await accessToken()
      if (!token) {
        throw new LabWeaverApiError('LW_SDK_AUTH_TOKEN_UNAVAILABLE', 'A current OIDC bearer token is required.')
      }
      // Only HTTP bearer schemes receive the token; the browser sends the BFF cookie itself.
      const auth = (scheme: Auth) => (scheme.type === 'http' && scheme.scheme === 'bearer' ? token : undefined)
      return open({ ...options, auth })
    }
  }
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

attachAuthInterceptor(
  sdkTransport,
  API_AUTH_MODE === 'bff'
    ? { mode: 'bff' }
    : { mode: 'bearer', accessToken: getOidcAccessToken },
)
attachResponseInterceptor(sdkTransport, API_AUTH_MODE === 'bff' ? reportSessionRejected : undefined)

/** Initialize the generated SDK client transport before the Vue app mounts. */
export async function initializeSdkClient(): Promise<void> {
  // Make the generated SDK functions use the configured SDK transport.
  defaultSdkClient.setConfig({
    axios: sdkTransport,
    baseURL: '',
  })
  // BFF streams carry the session cookie, which same-origin fetch sends itself.
  if (API_AUTH_MODE === 'bearer') requireStreamBearer(defaultSdkClient, getOidcAccessToken)
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
