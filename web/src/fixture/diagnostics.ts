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
      instance: '/fixture',
      requestId: 'fixture-request',
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

export function forbidden(detail = '当前角色没有权限执行此操作'): FixtureResponse<ProblemDetails> {
  return problem(403, 'FORBIDDEN', detail, false)
}

export function missingHeader(header: string): FixtureResponse<ProblemDetails> {
  return problem(400, 'FIXTURE_MISSING_HEADER', `请求缺少必需 header：${header}`, false)
}

export function conflict(detail: string): FixtureResponse<ProblemDetails> {
  return problem(409, 'FIXTURE_CONFLICT', detail, false)
}

export function preconditionFailed(detail: string): FixtureResponse<ProblemDetails> {
  return problem(412, 'PRECONDITION_FAILED', detail, false)
}

export function unprocessable(detail: string): FixtureResponse<ProblemDetails> {
  return problem(422, 'UNPROCESSABLE_ENTITY', detail, false)
}
