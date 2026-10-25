import axios, { AxiosError, type AxiosInstance } from 'axios'
import { API_AUTH_MODE, API_BASE_URL } from '@/config'
import { getOidcAccessToken } from '@/composables/useAuth'
import type { ProblemDetails } from '@/generated/contracts'
import { createClient, type Client } from '@/generated/contracts/client'
import { client as defaultSdkClient } from '@/generated/contracts/client.gen'
import type { ServerSentEventsOptions, ServerSentEventsResult } from '@/generated/contracts/core/serverSentEvents.gen'
import { REQUEST_ID_HEADER, rememberHeaderRequestId } from './requestId'
import {
  IdentityWaitInterrupted,
  identityReadPending,
  identityReadsSettled,
  reportSessionRejected,
  sessionActor,
  sessionActorInDoubt,
  sessionCredentialVersion,
  sessionEpoch,
  sessionEpochSignal,
} from './session'

export type AccessTokenProvider = () => Promise<string | undefined>
/** A CSRF token and the actor of the BFF session that issued it. */
export interface CsrfToken {
  token: string
  actorId: string
}
/** What a CSRF lookup for one write may take: it stops when `signal` aborts, at the latest at `deadline` (epoch ms). */
export interface CsrfTokenRequest {
  signal: AbortSignal
  deadline: number
}
export type CsrfTokenProvider = (request: CsrfTokenRequest) => Promise<CsrfToken | undefined>

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
  | 'LW_SDK_IDENTITY_UNCONFIRMED'

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
  // A token is kept with the actor of the session that issued it. An epoch
  // change or any CSRF/session rejection changes the credential version and
  // retires it, so the next write asks the BFF which session the cookie holds.
  let cached: { csrf: CsrfToken; expiresAt: number; version: string } | undefined
  // One lookup serves every write of the same credential version; it is
  // aborted once all of them stopped waiting (cancelled or past their deadline).
  let inFlight: { csrf: Promise<CsrfToken>; version: string; abort: AbortController; waiters: number } | undefined

  function lookUp(version: string) {
    const abort = new AbortController()
    const csrf = axios
      .get('/api/v1/auth/csrf', {
        baseURL: baseUrl,
        timeout,
        signal: abort.signal,
        withCredentials: true,
        headers: { Accept: 'application/json, application/problem+json' },
      })
      .then((response) => {
        const body = response.data as Record<string, unknown>
        const expiresAt = typeof body.expiresAt === 'string' ? Date.parse(body.expiresAt) : Number.NaN
        if (
          typeof body.csrfToken !== 'string' || !body.csrfToken ||
          typeof body.actorId !== 'string' || !body.actorId ||
          !Number.isFinite(expiresAt)
        ) {
          throw new LabWeaverApiError('LW_SDK_PROBLEM_INVALID', 'The BFF returned an invalid CSRF response.')
        }
        const issued = { token: body.csrfToken, actorId: body.actorId }
        // A token issued while the session changed is not kept for later writes.
        if (sessionCredentialVersion() === version) cached = { csrf: issued, expiresAt, version }
        return issued
      })
      .finally(() => {
        if (inFlight === lookup) inFlight = undefined
      })
    const lookup = { csrf, version, abort, waiters: 0 }
    inFlight = lookup
    return lookup
  }

  return async ({ signal }) => {
    const version = sessionCredentialVersion()
    if (cached?.version === version && cached.expiresAt - Date.now() > 30_000) return cached.csrf
    const lookup = inFlight?.version === version ? inFlight : lookUp(version)
    lookup.waiters += 1
    return new Promise<CsrfToken>((resolve, reject) => {
      let waiting = true
      const leave = () => {
        waiting = false
        signal.removeEventListener('abort', stop)
        lookup.waiters -= 1
      }
      const stop = () => {
        if (!waiting) return
        leave()
        if (lookup.waiters === 0) {
          lookup.abort.abort()
          if (inFlight === lookup) inFlight = undefined
        }
        reject(new LabWeaverApiError('LW_SDK_REQUEST_CANCELLED', 'The CSRF token lookup was abandoned.'))
      }
      signal.addEventListener('abort', stop)
      if (signal.aborted) stop()
      lookup.csrf.then(
        (csrf) => {
          if (!waiting) return
          leave()
          resolve(csrf)
        },
        (error: unknown) => {
          if (!waiting) return
          leave()
          reject(error)
        },
      )
    })
  }
}

/**
 * When a request entered the SDK: its session epoch and start time, fixed
 * synchronously at the call, before any generated step (security, request
 * validator, serializer) awaits. Not a plain object, so Axios carries this
 * same frozen instance into the request config instead of copying it.
 */
class CallStart {
  constructor(readonly epoch: number, readonly startedAt: number) {
    Object.freeze(this)
  }
}

const CALL_START = 'labweaverCallStart'

function callStartOf(config: object): CallStart | undefined {
  const start = (config as Record<string, unknown>)[CALL_START]
  return start instanceof CallStart ? start : undefined
}

const REQUEST_METHODS = ['connect', 'delete', 'get', 'head', 'options', 'patch', 'post', 'put', 'trace', 'request'] as const

/**
 * Every REST call of `client` records its `CallStart` when it is entered. The
 * generated SDK functions call these methods on the client object and pass
 * unknown options through to Axios, so the stamp reaches the interceptors.
 */
function stampCallStart(client: Client): void {
  const methods = client as unknown as Record<(typeof REQUEST_METHODS)[number], (options: Record<string, unknown>) => unknown>
  for (const method of REQUEST_METHODS) {
    const send = methods[method]
    methods[method] = (options) => send({ ...options, [CALL_START]: new CallStart(sessionEpoch.value, Date.now()) })
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

/** A write refused because the actor is unknown; nothing was sent. */
function identityUnconfirmed(): LabWeaverApiError {
  const refused = new LabWeaverApiError('LW_SDK_IDENTITY_UNCONFIRMED', 'The signed-in actor could not be confirmed; the write was not sent.')
  // The generated SDK hands `response.data` back as `result.error`: give
  // callers the code (and its Chinese reason) instead of an empty object.
  Object.assign(refused, { response: { data: { diagnosticCode: refused.diagnosticCode, detail: refused.message, retryable: false } } })
  return refused
}

/**
 * The caller's cancellation and the write's remaining deadline (counted from
 * the SDK call) as one signal, for every wait before the write is sent.
 */
function writeWaits(config: { timeout?: number; signal?: unknown }, startedAt: number) {
  const timeout = typeof config.timeout === 'number' && config.timeout > 0 ? config.timeout : undefined
  const deadline = timeout === undefined ? Number.POSITIVE_INFINITY : startedAt + timeout
  const stopped = new AbortController()
  let reason: 'timeout' | 'cancelled' | undefined
  const stop = (why: 'timeout' | 'cancelled') => {
    if (reason) return
    reason = why
    stopped.abort()
  }
  const caller = config.signal as Pick<AbortSignal, 'aborted'> & Partial<Pick<AbortSignal, 'addEventListener' | 'removeEventListener'>> | undefined
  const cancel = () => stop('cancelled')
  let timer: ReturnType<typeof setTimeout> | undefined
  if (caller?.aborted) cancel()
  else caller?.addEventListener?.('abort', cancel)
  if (Number.isFinite(deadline)) {
    const remaining = deadline - Date.now()
    if (remaining <= 0) stop('timeout')
    else timer = setTimeout(() => stop('timeout'), remaining)
  }
  const interrupted = () => reason === 'timeout'
    ? new LabWeaverApiError('LW_SDK_REQUEST_TIMEOUT', 'The API request timed out before it was sent.')
    : new LabWeaverApiError('LW_SDK_REQUEST_CANCELLED', 'The API request was cancelled before it was sent.')
  /** Run `work` unless already stopped; a stop rejects at once, whether or not `work` honors the signal. */
  const until = <T>(work: () => Promise<T>): Promise<T> => {
    if (stopped.signal.aborted) return Promise.reject(interrupted())
    return new Promise<T>((resolve, reject) => {
      let settled = false
      // The one way out (result, failure, stop or a synchronous throw): the
      // stop listener goes at once, and a late answer of `work` changes nothing.
      const settle = (outcome: () => void) => {
        if (settled) return
        settled = true
        stopped.signal.removeEventListener('abort', onStop)
        outcome()
      }
      const onStop = () => settle(() => reject(interrupted()))
      stopped.signal.addEventListener('abort', onStop)
      let pending: Promise<T>
      try {
        pending = work()
      } catch (error) {
        settle(() => reject(error))
        return
      }
      pending.then(
        (value) => settle(() => resolve(value)),
        (error: unknown) => settle(() => reject(stopped.signal.aborted || error instanceof IdentityWaitInterrupted ? interrupted() : error)),
      )
    })
  }
  return {
    signal: stopped.signal,
    deadline,
    until,
    dispose() {
      clearTimeout(timer)
      caller?.removeEventListener?.('abort', cancel)
    },
  }
}

/**
 * The one authoritative check of a write, run synchronously as the last thing
 * before the real adapter: still the epoch the write started in (otherwise it
 * belongs to the previous actor), no identity read in flight, the actor not in
 * doubt, and its CSRF token issued to the confirmed actor. Nothing is awaited
 * between this check and the dispatch. A refusal sends nothing and is not
 * retried; the caller keeps its pending command for the user to resubmit.
 */
function assertWriteStillAuthorized(initiatingEpoch: number, csrfActor: string): void {
  if (sessionEpoch.value !== initiatingEpoch) throw sessionChanged()
  if (identityReadPending() || sessionActorInDoubt.value) throw identityUnconfirmed()
  const confirmedActor = sessionActor()
  if (confirmedActor === undefined) throw identityUnconfirmed()
  // The BFF accepts a token only with the session that issued it; a token of
  // another actor means the cookie changed hands: read the actor again.
  if (csrfActor !== confirmedActor) {
    reportSessionRejected()
    throw sessionChanged()
  }
}

function attachAuthInterceptor(instance: AxiosInstance, authentication: LabWeaverApiAuthentication): void {
  const csrfToken =
    authentication.mode === 'bff'
      ? (authentication.csrfToken ?? createBffCsrfTokenProvider(instance.defaults.baseURL ?? '', instance.defaults.timeout as number))
      : undefined

  instance.interceptors.request.use(async (config) => {
    const start = callStartOf(config)
    const unsafe = isUnsafeMethod(config.method)
    // A write is only ever checked against the epoch its SDK call was entered in.
    if (csrfToken && unsafe && !start) {
      throw new LabWeaverApiError('LW_SDK_CONFIGURATION_INVALID', 'A write must be started through a LabWeaver SDK client.')
    }
    const initiatingEpoch = start?.epoch ?? sessionEpoch.value
    requestEpochs.set(config, initiatingEpoch)
    if (startedInEarlierSession(config)) throw sessionChanged()
    if (authentication.mode === 'bearer') {
      const token = await authentication.accessToken()
      if (!token) {
        throw new LabWeaverApiError('LW_SDK_AUTH_TOKEN_UNAVAILABLE', 'A current OIDC bearer token is required.')
      } else {
        config.headers.Authorization = `Bearer ${token}`
      }
    } else if (csrfToken && start && unsafe) {
      // Earlier steps may wait, within the caller's cancellation and the
      // write's deadline: for an identity read in flight, then for the token.
      const waits = writeWaits(config, start.startedAt)
      let csrf: CsrfToken | undefined
      try {
        // Only a head start: the final check before dispatch decides, so a read that starts after this wait still stops the write.
        await waits.until(() => identityReadsSettled({ signal: waits.signal }))
        if (sessionActorInDoubt.value) throw identityUnconfirmed()
        csrf = await waits.until(() => csrfToken({ signal: waits.signal, deadline: waits.deadline }))
      } finally {
        waits.dispose()
      }
      if (!csrf) {
        throw new LabWeaverApiError('LW_SDK_AUTH_TOKEN_UNAVAILABLE', 'A current BFF CSRF token is required.')
      }
      const csrfActor = csrf.actorId
      config.headers['X-CSRF-Token'] = csrf.token
      const realAdapter = axios.getAdapter(config.adapter ?? axios.defaults.adapter)
      config.adapter = (dispatched) => {
        assertWriteStillAuthorized(initiatingEpoch, csrfActor)
        return realAdapter(dispatched)
      }
    }
    // Reading the bearer of another actor advances the epoch.
    if (startedInEarlierSession(config)) throw sessionChanged()
    return config
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
  stampCallStart(client)
  guardStreams(client, authentication.mode === 'bearer' ? authentication.accessToken : undefined)
  return client
}

/** Generated SSE passes these through to its fetch loop; the request options type does not list them. */
type StreamOptions = Parameters<Client['sse']['get']>[0] & Pick<ServerSentEventsOptions, 'fetch' | 'sseSleepFn'>
type OpenStream = (options: StreamOptions) => Promise<ServerSentEventsResult>

/**
 * Generated SSE opens and reopens streams with fetch, outside the Axios
 * interceptors. Every fetch of a stream (the first one, made on the first
 * `next()`, and each reconnect) happens only in the session epoch the stream
 * was opened in and, in bearer mode, with a current token read just before it.
 * A stream whose epoch ended delivers nothing more and does not reconnect; the
 * browser sends the BFF cookie itself.
 */
function guardStreams(client: Client, accessToken?: AccessTokenProvider): void {
  const sse = client.sse as Record<keyof Client['sse'], OpenStream>
  for (const method of Object.keys(sse) as (keyof Client['sse'])[]) {
    const open = sse[method]
    sse[method] = async (options) => {
      const epochEnded = sessionEpochSignal()
      // Reading the bearer of another actor advances the epoch, so the check after it covers the token's actor.
      const currentBearer = async () => {
        const token = accessToken ? await accessToken() : undefined
        if (accessToken && !token) {
          throw new LabWeaverApiError('LW_SDK_AUTH_TOKEN_UNAVAILABLE', 'A current OIDC bearer token is required.')
        }
        if (epochEnded.aborted) throw sessionChanged()
        return token
      }
      // Fails before the stream exists, so no request is ever made without a bearer.
      let openingBearer = await currentBearer()

      // The one end of the stream, however it ends: completion, error, the
      // caller's cancellation, `return()` or the end of its session epoch. It
      // releases the listeners on the caller's and the epoch's signals.
      const stopped = new AbortController()
      const callerSignal = options.signal ?? undefined
      let wakeFromBackoff: (() => void) | undefined
      const stop = () => {
        callerSignal?.removeEventListener?.('abort', stop)
        epochEnded.removeEventListener('abort', stop)
        stopped.abort()
        wakeFromBackoff?.()
      }
      if (callerSignal?.aborted || epochEnded.aborted) {
        stop()
      } else {
        callerSignal?.addEventListener?.('abort', stop)
        epochEnded.addEventListener('abort', stop)
      }
      /** `Promise.withResolvers` is outside the project's ES2020 lib; a backoff ends early once the stream stops. */
      const backoff = (ms: number) => new Promise<void>((resolve) => {
        const timer = setTimeout(() => {
          wakeFromBackoff = undefined
          resolve()
        }, ms)
        wakeFromBackoff = () => {
          clearTimeout(timer)
          wakeFromBackoff = undefined
          resolve()
        }
      })

      // A stream that cannot be opened (a request validator or preflight
      // rejecting) releases its listeners and fails with the original error.
      const { stream } = await open({
        ...options,
        signal: stopped.signal,
        // Events of an ended epoch are neither reported nor delivered.
        onSseEvent: options.onSseEvent && ((event) => {
          if (!stopped.signal.aborted) options.onSseEvent?.(event)
        }),
        sseSleepFn: async (ms) => {
          if (!stopped.signal.aborted) await (options.sseSleepFn ?? backoff)(ms)
        },
        fetch: async (input, init) => {
          try {
            if (epochEnded.aborted) throw sessionChanged()
            const bearer = openingBearer ?? (await currentBearer())
            openingBearer = undefined
            const request = new Request(input, init)
            if (bearer) request.headers.set('Authorization', `Bearer ${bearer}`)
            return await (options.fetch ?? globalThis.fetch)(request)
          } catch (error) {
            // The generated loop retries failed fetches until its signal aborts.
            if (error instanceof LabWeaverApiError) stop()
            throw error
          }
        },
      }).catch((error: unknown) => {
        stop()
        throw error
      })

      const close = async (): Promise<IteratorResult<unknown, void>> => {
        stop()
        await stream.return(undefined)
        return { done: true, value: undefined }
      }
      // One network chunk can hold several events, which the generated loop
      // hands out without reading again; the epoch is therefore also checked
      // at every delivery, not only before each fetch.
      const advance = async (step: () => Promise<IteratorResult<unknown, void>>) => {
        try {
          const result = await step()
          return result.done || stopped.signal.aborted ? close() : result
        } catch (error) {
          await close()
          throw error
        }
      }
      const delivered: AsyncGenerator<unknown, void, unknown> = {
        next: async () => (stopped.signal.aborted ? close() : advance(() => stream.next())),
        return: close,
        // Forwarded so the generated loop reports the error and applies its own
        // retry rules; a stream of an ended epoch is closed and rethrows.
        async throw(error: unknown) {
          if (stopped.signal.aborted) {
            await close()
            throw error
          }
          return advance(() => stream.throw(error))
        },
        [Symbol.asyncIterator]() {
          return this
        },
      }
      return { stream: delivered }
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
  stampCallStart(defaultSdkClient)
  guardStreams(defaultSdkClient, API_AUTH_MODE === 'bearer' ? getOidcAccessToken : undefined)
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
