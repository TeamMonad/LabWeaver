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
