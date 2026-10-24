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
      return '这个环境的资源使用授权已到期、被回收或尚未生效，不能继续创建、启动、重启、恢复或升级。'
    case 'LW_ENVIRONMENT_ALLOCATION_PENDING':
      return '资源申请已批准，平台正在分配资源并创建这个环境，环境还没有就绪。'
    case 'LW_ENVIRONMENT_LEASE_VERIFICATION_RESPONSE_INVALID':
      return '平台暂时无法核验这个 Work 的资源使用授权，本次操作没有执行。'
    case 'LW_WORK_TEMPLATE_UPGRADE_UNSUPPORTED_FOR_VM':
      return '虚拟机 Work 不支持在原 Work 内升级模板：需要导出/导入流程，当前不可用。'
    case 'LW_WORK_TEMPLATE_UPGRADE_UNAVAILABLE':
      return '本版本暂不支持在原资金租约上升级模板。可以用新版本新建 Work，再自行迁移 /workspace 数据。'
    case 'LW_WORK_TEMPLATE_UPGRADE_REQUIRES_RESOURCE_APPROVAL':
      return '目标版本的资源、网络、安全或入口配置与当前 Work 不同，需要重新申请资源，不能在原 Work 内升级。'
    // Model input selection and authorization codes (#216).
    case 'LW_MODEL_INPUT_SELECTION_REQUIRED':
      return '没有收到有效的文件选择：必须勾选至少一个材料包内的文件，且不能重复或包含该材料包版本中不存在的路径。本次没有创建生成任务。'
    case 'LW_MODEL_INPUT_AUTHORIZATION_INVALID':
      return '评分规则的文件身份无效，平台没有保存这次修改。'
    case 'LW_MODEL_INPUT_AUTHORIZATION_DENIED':
      return '当前会话或账号权限不允许把所选内容交给模型，平台没有发起新的模型调用。'
    case 'LW_MODEL_INPUT_AUTHORIZATION_REQUIRED':
      return '任务缺少原始的模型输入授权，未开始新的模型调用。'
    case 'LW_MODEL_INPUT_AUTHORIZATION_UNAVAILABLE':
      return '平台暂时无法核验模型输入授权，未开始模型调用。'
    // Costing and Credits codes from Resource (#209/#210).
    case 'LW_RESOURCE_RATE_UNCONFIGURED':
      return '缺少适用费率：当前规格或时间段没有可计价的费率，估价或扣收无法完成，不会按免费处理。'
    case 'LW_RESOURCE_RATE_CURRENCY_CONFLICT':
      return '适用费率的币种不一致，无法形成统一的 CREDITS 金额。'
    case 'LW_RESOURCE_RATE_OVERLAP':
      return '同一计费维度的费率时间段重叠，无法确定该用哪一个费率。'
    case 'LW_RESOURCE_RATE_DIMENSION_INVALID':
      return '费率维度不完整：构建费率必须填写构建器标识，模型 token 费率必须填写模型标识和单次最大预占，其他费率不能填写这些字段。'
    case 'LW_RESOURCE_GPU_CATALOG_MISSING':
      return '所选 GPU 类型不在当前 GPU 目录中，当前不可用，无法计价或分配。'
    case 'LW_RESOURCE_GPU_CATALOG_AMBIGUOUS':
      return 'GPU 目录中同一类型对应多种分配方式，无法确定计价方式。'
    case 'LW_RESOURCE_GPU_CATALOG_INVALID':
      return 'GPU 目录配置无效，当前无法使用 GPU 计价。'
    case 'LW_RESOURCE_ESTIMATE_CHANGED':
      return '规格或费率在估价后发生了变化，原估价已失效。'
    case 'LW_CREDITS_INSUFFICIENT':
      return '项目 Credits 可用余额不足，本次操作没有执行。已预占的资金不能用于转出或新的申请。'
    case 'LW_CREDITS_CONFLICT':
      return '这笔 Credits 操作与已有记录冲突：可能已提交过、原账目已冲正，或涉及的资金已被预占或消费。'
    case 'LW_CREDITS_SCOPE_DENIED':
      return '当前账号无权查看或修改这个项目的 Credits。普通成员只能查看自己所在项目；发放、转移、调整和冲正只能由平台管理员执行。'
    case 'LW_CREDITS_NOT_FOUND':
      return '没有找到对应的项目账户或账目。'
    case 'LW_CREDITS_INVALID':
    case 'LW_CREDITS_PRECISION':
      return 'Credits 金额或原因无效：金额必须是最多六位小数的数值，原因不能为空。'
    case 'LW_CREDITS_STORAGE':
    case 'LW_CREDITS_PERSISTENCE':
      return 'Credits 账本暂时无法写入或读取，本次操作没有完成。'
    // Browser session codes (#198): the request was not carried out for the current actor.
    case 'LW_AUTH_CSRF_REJECTED':
      return '登录会话已在其他页面变化或安全校验已失效，这次操作没有执行。'
    case 'LW_SDK_SESSION_CHANGED':
      return '登录账号已变化，这次操作属于之前的账号，结果已丢弃。'
    case 'LW_SDK_SESSION_UNCONFIRMED':
      return '暂时无法确认当前登录账号，这次操作没有发送。'
    default:
      return message
  }
}

/** Next step tied to a specific diagnostic code; it takes precedence over the HTTP-status fallback. */
function codeNextStep(code: string): string | undefined {
  switch (code) {
    case 'LW_RUNTIME_LEASE_NOT_ACTIVE':
      return '请提交新的资源申请；申请审批通过后，平台会创建新的环境。'
    case 'LW_ENVIRONMENT_ALLOCATION_PENDING':
      return '请稍候片刻再点击“重试”；重试沿用同一请求，不会重复创建环境。'
    case 'LW_ENVIRONMENT_LEASE_VERIFICATION_RESPONSE_INVALID':
      return '请稍后重试；多次失败请把下方诊断信息发给管理员。'
    case 'LW_WORK_TEMPLATE_UPGRADE_UNSUPPORTED_FOR_VM':
      return '如需使用新版本，请用新版本模板新建 Work，再自行把需要的数据迁移过去。'
    case 'LW_WORK_TEMPLATE_UPGRADE_UNAVAILABLE':
    case 'LW_WORK_TEMPLATE_UPGRADE_REQUIRES_RESOURCE_APPROVAL':
      return '请用新版本模板新建 Work，迁移并检查数据后再释放原 Work。'
    case 'LW_MODEL_INPUT_SELECTION_REQUIRED':
      return '请刷新材料包后重新勾选要交给模型的文件，再启动生成。'
    case 'LW_MODEL_INPUT_AUTHORIZATION_INVALID':
      return '请刷新评分规则列表，按当前发布版本重新操作。'
    case 'LW_MODEL_INPUT_AUTHORIZATION_DENIED':
    case 'LW_MODEL_INPUT_AUTHORIZATION_REQUIRED':
      return '请确认仍有该项目的相应权限且登录未过期；需要时重新登录后再操作。'
    case 'LW_MODEL_INPUT_AUTHORIZATION_UNAVAILABLE':
      return '请稍后新建任务；多次失败请把下方诊断信息发给管理员。'
    case 'LW_RESOURCE_ESTIMATE_CHANGED':
      return '请重新估价，确认新的金额后再继续。'
    case 'LW_CREDITS_INSUFFICIENT':
      return '请联系平台管理员为项目发放 Credits，或等待已有资源回收释放预占后再试。'
    case 'LW_CREDITS_CONFLICT':
      return '请先刷新账目确认这笔操作是否已经生效，不要换一笔新操作重复提交。'
    case 'LW_AUTH_CSRF_REJECTED':
    case 'LW_SDK_SESSION_CHANGED':
      return '平台已重新确认当前登录账号。请核对页面上的账号和内容，再手动重新提交；平台不会自动重试。'
    case 'LW_SDK_SESSION_UNCONFIRMED':
      return '请检查网络后重试；仍失败请刷新页面或重新登录。'
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
