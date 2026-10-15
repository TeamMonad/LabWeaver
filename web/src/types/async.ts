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

function readableDiagnosticMessage(code: string, message: string): string {
  switch (code) {
    case 'LW_MATERIAL_RETENTION_EXPIRED':
      return '材料保留期限已过期。请重新上传材料，并重新生成、审批和发布；重试当前过期版本无法恢复。'
    case 'LW_RESOURCE_GPU_CAPACITY_EXHAUSTED':
      return 'GPU 容量当前不可用，可能已被其他任务占用。请稍后重试或调整资源申请。'
    case 'LW_RUNTIME_ALLOCATION_PATH_UNSUPPORTED':
      return '实验环境创建尚未接通原 ResourceRequest→批准→lease→handoff 路径。已拨款的 Experiment lease 走旧 reservation 会返回 422；请等待统一资金/handoff 流程就绪后再创建。'
    case 'LW_RESOURCE_RATE_UNCONFIGURED':
      return '缺少适用费率：估价或结算无法完成，不会按免费处理。'
    case 'LW_RESOURCE_GPU_CATALOG_MISSING':
      return '缺少 GPU 目录项：无法完成资源计价。'
    case 'LW_RESOURCE_ESTIMATE_CHANGED':
      return '报价已过期或规格/费率已变化：请重新估价后再继续。'
    default:
      return message
  }
}

export function makeDiagnostic(code: string, message: string, retryable = false): DiagnosticViewModel {
  return { code, message: readableDiagnosticMessage(code, message), retryable }
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
