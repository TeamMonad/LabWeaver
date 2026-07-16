import axios, { AxiosError, type AxiosRequestConfig } from 'axios'
import { API_AUTH_MODE, API_BASE_URL } from '@/config'
import { getOidcAccessToken } from '@/composables/useAuth'
import type { ProblemDetails } from '@/generated/contracts'
import { createClient, type Client } from '@/generated/contracts/client'

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
  if (raw === '/') return '/'
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
  let cached: { token: string; expiresAt: number } | undefined
  let inFlight: Promise<string | undefined> | undefined
  return async () => {
    if (cached && cached.expiresAt - Date.now() > 30_000) return cached.token
    if (inFlight) return inFlight
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
        cached = { token: body.csrfToken, expiresAt }
        return cached.token
      })
      .finally(() => {
        inFlight = undefined
      })
    return inFlight
  }
}

/** Strictly decodes the RFC 9457 extension shared by all generated SDK calls. */
export function decodeProblemDetails(error: unknown): ProblemDetails | undefined {
  if (error instanceof LabWeaverApiError) return error.problem
  if (!axios.isAxiosError(error)) return undefined
  return isProblemDetails(error.response?.data) ? error.response.data : undefined
}

/** The only supported Public SDK initialization path for browser consumers. */
export function createLabWeaverApiClient(options: LabWeaverApiClientOptions): Client {
  const timeout = options.timeoutMilliseconds ?? 30_000
  if (!Number.isSafeInteger(timeout) || timeout <= 0 || timeout > 120_000) {
    throw new LabWeaverApiError('LW_SDK_CONFIGURATION_INVALID', 'API timeout must be between 1 and 120000 ms.')
  }

  const baseUrl = normalizedBaseUrl(options.baseUrl)
  const authentication = options.authentication
  const csrfToken =
    authentication.mode === 'bff'
      ? (authentication.csrfToken ?? createBffCsrfTokenProvider(baseUrl, timeout))
      : undefined
  const instance = axios.create({
    baseURL: baseUrl,
    timeout,
    withCredentials: authentication.mode === 'bff',
    headers: { Accept: 'application/json, application/problem+json' },
  })

  instance.interceptors.request.use(async (config) => {
    if (authentication.mode === 'bearer') {
      const token = await authentication.accessToken()
      if (!token) {
        throw new LabWeaverApiError('LW_SDK_AUTH_TOKEN_UNAVAILABLE', 'A current OIDC bearer token is required.')
      }
      config.headers.Authorization = `Bearer ${token}`
    } else if (isUnsafeMethod(config.method)) {
      const token = await csrfToken?.()
      if (!token) {
        throw new LabWeaverApiError('LW_SDK_AUTH_TOKEN_UNAVAILABLE', 'A current BFF CSRF token is required.')
      }
      config.headers['X-CSRF-Token'] = token
    }
    return config
  })

  instance.interceptors.response.use(
    (response) => response,
    (error: unknown) => {
      if (error instanceof LabWeaverApiError) return Promise.reject(error)
      if (!axios.isAxiosError(error)) {
        return Promise.reject(new LabWeaverApiError('LW_SDK_TRANSPORT_FAILED', 'The API transport failed.'))
      }
      if (axios.isCancel(error) || error.code === AxiosError.ERR_CANCELED) {
        return Promise.reject(new LabWeaverApiError('LW_SDK_REQUEST_CANCELLED', 'The API request was cancelled.'))
      }
      if (error.code === AxiosError.ECONNABORTED) {
        return Promise.reject(new LabWeaverApiError('LW_SDK_REQUEST_TIMEOUT', 'The API request timed out.'))
      }
      const contentType = String(error.response?.headers?.['content-type'] ?? '')
      if (isProblemDetails(error.response?.data)) {
        const problem = error.response.data
        return Promise.reject(new LabWeaverApiError(problem.diagnosticCode, problem.detail, problem))
      }
      if (contentType.includes('application/problem+json')) {
        return Promise.reject(
          new LabWeaverApiError('LW_SDK_PROBLEM_INVALID', 'The server returned malformed RFC 9457 Problem Details.'),
        )
      }
      return Promise.reject(new LabWeaverApiError('LW_SDK_TRANSPORT_FAILED', 'The API transport failed.'))
    },
  )

  return createClient({
    axios: instance,
    baseURL: baseUrl,
    timeout,
  })
}

export const apiClient = createLabWeaverApiClient({
  baseUrl: API_BASE_URL,
  authentication:
    API_AUTH_MODE === 'bff'
      ? { mode: 'bff' }
      : { mode: 'bearer', accessToken: getOidcAccessToken },
})

/** Health checks are intentionally outside the authenticated Public API contract. */
export async function healthCheck(config?: AxiosRequestConfig) {
  return axios.get('/health/live', config)
}
