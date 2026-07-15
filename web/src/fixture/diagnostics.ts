import type { ProblemDetails } from '@/generated/contracts'
import type { FixtureResponse } from './types'

export function problem(status: number, code: string, detail: string, retryable = false): FixtureResponse<ProblemDetails> {
  return {
    status,
    data: {
      type: 'about:blank',
      title: code,
      status,
      detail,
      diagnosticCode: code,
      retryable,
    },
    headers: { 'Content-Type': 'application/problem+json' },
  }
}

export function notFound(path: string): FixtureResponse<ProblemDetails> {
  return problem(404, 'FIXTURE_ROUTE_NOT_FOUND', `fixture 中未建模的请求：${path}`, false)
}

export function unauthorized(detail = '缺少或无效的 Authorization header'): FixtureResponse<ProblemDetails> {
  return problem(401, 'UNAUTHENTICATED', detail, false)
}

export function missingHeader(header: string): FixtureResponse<ProblemDetails> {
  return problem(400, 'FIXTURE_MISSING_HEADER', `请求缺少必需 header：${header}`, false)
}
