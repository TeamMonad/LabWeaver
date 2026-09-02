import type { ProblemDetails } from '@/generated/contracts'

export interface DiagnosticViewModel {
  code: string
  traceId?: string
  message: string
  retryable: boolean
}

export type AsyncState<T> =
  | { kind: 'idle' }
  | { kind: 'loading'; message?: string }
  | { kind: 'success'; data: T }
  | { kind: 'empty' }
  | { kind: 'error'; diagnostic: DiagnosticViewModel }
  | { kind: 'blocked'; diagnostic: DiagnosticViewModel }
  | { kind: 'timeout'; diagnostic: DiagnosticViewModel }
  | { kind: 'conflict'; diagnostic: DiagnosticViewModel }
  | { kind: 'unauthorized'; diagnostic: DiagnosticViewModel }
  | { kind: 'revoked'; diagnostic: DiagnosticViewModel }
  | { kind: 'sse-gap'; diagnostic: DiagnosticViewModel }

export function makeDiagnostic(code: string, message: string, retryable = false): DiagnosticViewModel {
  return { code, message, retryable }
}

/**
 * Extract RFC 9457 ProblemDetails from a generated-SDK error result.
 *
 * The hey-api axios transport exposes the response body as `result.error`
 * (already the ProblemDetails object). Some call sites instead hold the raw
 * AxiosError (e.g. throwOnError flows), where the body lives at
 * `error.response.data`. Both shapes are normalized here; anything that is
 * not a ProblemDetails body (transport errors, cancellations) returns
 * undefined so callers fall back to their own diagnostic.
 */
export function extractProblemDetails(error: unknown): ProblemDetails | undefined {
  if (!error || typeof error !== 'object') return undefined
  const axiosBody = (error as { response?: { data?: unknown } }).response?.data
  const candidate = (axiosBody && typeof axiosBody === 'object' ? axiosBody : error) as Partial<ProblemDetails>
  if (typeof candidate.diagnosticCode !== 'string' && typeof candidate.detail !== 'string') {
    return undefined
  }
  return candidate as ProblemDetails
}
