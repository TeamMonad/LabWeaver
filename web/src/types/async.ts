import type { ProblemDetails } from '@/generated/contracts'
import { headerRequestId } from '@/api/requestId'

export interface DiagnosticViewModel {
  code: string
  traceId?: string
  /**
   * Server request identifier: the RFC 9457 body `requestId`, or else the
   * `x-request-id` response header. Never generated in the browser.
   */
  requestId?: string
  /** HTTP status of the failed request, when the server answered. */
  status?: number
  message: string
  /** Chinese next step shown after the reason. */
  nextStep?: string
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
    // Codes declared in `x-labweaver-errors` of the project and membership operations (#199).
    case 'LW_PROJECT_GOVERNANCE_DENIED':
      // Control uses one code for several governance rules, so the reason lists them instead of guessing one.
      return '项目治理规则不允许这项操作：你的项目职责不足，或操作对象是你自己、项目负责人或平台管理员身份。项目负责人职责只能在创建项目时确定，归档项目只能由负责人或平台管理员执行。'
    case 'LW_PROJECT_ARCHIVED':
      return '项目已归档，不能再修改项目或成员。'
    case 'LW_PROJECT_MEMBERSHIP_CONFLICT':
      return '该账号的成员记录已被其他操作修改。请刷新成员列表后再试。'
    case 'LW_PROJECT_MEMBERSHIP_NOT_FOUND':
      return '该成员记录不存在或已被移除。请刷新成员列表。'
    case 'LW_PROJECT_COURSE_MEMBERSHIP_REQUIRED':
      return '该账号还不是课程成员，不能加入课程项目。请先将其加入课程。'
    case 'LW_PROJECT_OWNER_MEMBERSHIP_PROTECTED':
      return '项目负责人的成员记录受保护，不能修改或移除。'
    case 'LW_ACCESS_DIRECTORY_USERNAME_INVALID':
      return '用户名格式无效。请输入完整的组织账号用户名。'
    case 'LW_ACCESS_DIRECTORY_USER_NOT_FOUND':
      return '没有找到这个账号。请确认用户名完整且正确。'
    case 'LW_ACCESS_DIRECTORY_USER_DISABLED':
      return '该账号已停用，不能加入项目。'
    // Work template, lease and Agent configuration codes (#202).
    case 'LW_AGENT_CONFIGURATION_UNREADABLE':
      return '平台无法读取项目的 Agent 配置。'
    case 'LW_RUNTIME_LEASE_NOT_ACTIVE':
      return '这个 Work 的资源使用授权已到期、被回收或尚未生效，不能继续启动、重启、恢复或升级。'
    case 'LW_ENVIRONMENT_LEASE_VERIFICATION_RESPONSE_INVALID':
      return '平台暂时无法核验这个 Work 的资源使用授权，本次操作没有执行。'
    case 'LW_WORK_TEMPLATE_UPGRADE_UNSUPPORTED_FOR_VM':
      return '虚拟机 Work 不支持在原 Work 内升级模板：需要导出/导入流程，当前不可用。'
    default:
      return message
  }
}

/** Next step tied to a specific diagnostic code; it takes precedence over the HTTP-status fallback. */
function codeNextStep(code: string): string | undefined {
  switch (code) {
    case 'LW_RUNTIME_LEASE_NOT_ACTIVE':
      return '请提交新的资源申请；申请审批通过后，平台会创建一个新的 Work。'
    case 'LW_ENVIRONMENT_LEASE_VERIFICATION_RESPONSE_INVALID':
      return '请稍后重试；多次失败请把下方诊断信息发给管理员。'
    case 'LW_WORK_TEMPLATE_UPGRADE_UNSUPPORTED_FOR_VM':
      return '如需使用新版本，请用新版本模板新建 Work，再自行把需要的数据迁移过去。'
    default:
      return undefined
  }
}

/**
 * Next step for a failed request, derived only from the HTTP status the
 * server returned. Domain-specific advice belongs in the server detail or in
 * `readableDiagnosticMessage`; this fallback never guesses a business cause.
 */
export function diagnosticNextStep(status: number | undefined, retryable: boolean): string | undefined {
  switch (status) {
    case 401:
      return '登录已失效，请重新登录后再试。'
    case 403:
      return '当前账号没有这项操作的权限。请确认所选项目，或联系项目 Owner 授权。'
    case 404:
      return '对象不存在或已被删除。请刷新列表，或确认所选项目是否正确。'
    case 409:
    case 412:
      return '数据已被其他操作更新。请刷新后确认最新状态，再决定是否重试。'
    case 410:
      return '对象已过期或已回收，无法继续使用。请重新创建。'
    case 422:
    case 400:
      return '请按提示修改输入后重新提交。'
    case 429:
      return '请求过于频繁，请稍等片刻再试。'
    default:
      if (status !== undefined && status >= 500) {
        return retryable ? '平台服务暂时不可用，请稍后重试；多次失败请把下方诊断信息发给管理员。' : '平台服务处理失败，请把下方诊断信息发给管理员。'
      }
      return undefined
  }
}

type DiagnosticProblem = Pick<Partial<ProblemDetails>, 'requestId' | 'status' | 'traceId'> | null | undefined

export function makeDiagnostic(code: string, message: string, retryable = false, problem?: DiagnosticProblem): DiagnosticViewModel {
  const diagnostic: DiagnosticViewModel = { code, message: readableDiagnosticMessage(code, message), retryable }
  const specificNextStep = codeNextStep(code)
  if (specificNextStep) diagnostic.nextStep = specificNextStep
  if (!problem) return diagnostic
  const requestId = typeof problem.requestId === 'string' && problem.requestId ? problem.requestId : headerRequestId(problem)
  if (requestId) diagnostic.requestId = requestId
  if (typeof problem.traceId === 'string' && problem.traceId) diagnostic.traceId = problem.traceId
  if (typeof problem.status === 'number') {
    diagnostic.status = problem.status
    const nextStep = specificNextStep ?? diagnosticNextStep(problem.status, retryable)
    if (nextStep) diagnostic.nextStep = nextStep
  }
  return diagnostic
}

/** Build a diagnostic from a generated-SDK error, keeping requestId and status for support. */
export function diagnosticFromError(error: unknown, fallbackCode: string, fallbackMessage: string, fallbackRetryable = true): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  if (problem) {
    const diagnostic = makeDiagnostic(problem.diagnosticCode ?? fallbackCode, problem.detail ?? fallbackMessage, problem.retryable ?? fallbackRetryable, problem)
    if (!diagnostic.requestId) {
      const fromHeader = headerRequestId(error)
      if (fromHeader) diagnostic.requestId = fromHeader
    }
    return diagnostic
  }
  const diagnostic = makeDiagnostic(fallbackCode, fallbackMessage, fallbackRetryable)
  const fromHeader = headerRequestId(error)
  if (fromHeader) diagnostic.requestId = fromHeader
  if (isTransportError(error)) diagnostic.nextStep = '无法连接平台服务。请检查网络连接后重试。'
  return diagnostic
}

function isTransportError(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false
  const candidate = error as { isAxiosError?: boolean; response?: unknown; code?: unknown }
  return candidate.isAxiosError === true && candidate.response === undefined
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
