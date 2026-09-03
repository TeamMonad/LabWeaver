/**
 * 中文状态/枚举标签映射。
 *
 * 所有英文枚举（状态机状态、协议、运行时等）在 UI 直出前必须经此处映射；
 * 技术标识符（ID、SHA-256、诊断码）保持原样。新状态加入时同步补充映射，
 * 未命中的值回退为原始英文枚举，保证新增状态可见而不静默。
 */

function labelOf(map: Record<string, string>, value: string | null | undefined): string {
  if (!value) return '—'
  return map[value] ?? value
}

const AGENT_RUN_STATE: Record<string, string> = {
  requested: '已受理',
  running: '运行中',
  partially_succeeded: '部分成功',
  succeeded: '已成功',
  failed: '失败',
  cancelling: '取消中',
  cancelled: '已取消',
}

export function agentRunStateLabel(value: string | null | undefined): string {
  return labelOf(AGENT_RUN_STATE, value)
}

const AGENT_ATTEMPT_STATE: Record<string, string> = {
  pending: '等待中',
  running: '执行中',
  repairing: 'Agent 修复中',
  succeeded: '成功',
  failed: '失败',
  cancelled: '已取消',
}

export function agentAttemptStateLabel(value: string | null | undefined): string {
  return labelOf(AGENT_ATTEMPT_STATE, value)
}

const AGENT_TRACK_KIND: Record<string, string> = {
  environment: '环境轨道',
  evaluation: '评测轨道',
}

export function agentTrackKindLabel(value: string | null | undefined): string {
  return labelOf(AGENT_TRACK_KIND, value)
}

const ENVIRONMENT_STATE: Record<string, string> = {
  requested: '已请求',
  validating: '校验中',
  building: '构建中',
  provisioning: '置备中',
  ready: '运行中',
  stopped: '已停止',
  updating: '更新中',
  expiring: '即将过期',
  deleting: '删除中',
  deleted: '已删除',
  failed: '失败',
}

export function environmentStateLabel(value: string | null | undefined): string {
  return labelOf(ENVIRONMENT_STATE, value)
}

const RESOURCE_REQUEST_STATE: Record<string, string> = {
  draft: '草稿',
  submitted: '已提交',
  policy_checked: '策略已通过',
  reviewing: '待审批',
  approved: '已批准',
  allocating: '分配中',
  active: '使用中',
  expiring: '即将到期',
  expired: '已到期',
  rejected: '已拒绝',
  revoked: '已撤销',
  failed: '失败',
}

export function resourceRequestStateLabel(value: string | null | undefined): string {
  return labelOf(RESOURCE_REQUEST_STATE, value)
}

const RESOURCE_LEASE_STATE: Record<string, string> = {
  active: '使用中',
  expiring: '即将到期',
  expired: '已到期',
  released: '已释放',
  revoked: '已撤销',
  failed: '失败',
}

export function resourceLeaseStateLabel(value: string | null | undefined): string {
  return labelOf(RESOURCE_LEASE_STATE, value)
}

const ACCESS_GRANT_STATE: Record<string, string> = {
  active: '生效中',
  expired: '已过期',
  revoked: '已撤销',
}

export function accessGrantStateLabel(value: string | null | undefined): string {
  return labelOf(ACCESS_GRANT_STATE, value)
}

const ENDPOINT_HEALTH: Record<string, string> = {
  healthy: '健康',
  unhealthy: '未就绪',
  removed: '已移除',
}

export function endpointHealthLabel(value: string | null | undefined): string {
  return labelOf(ENDPOINT_HEALTH, value)
}

const EVALUATION_STATE: Record<string, string> = {
  requested: '已请求',
  planning: '规划中',
  running: '运行中',
  aggregating: '汇总中',
  awaiting_teacher_review: '待教师复核',
  released: '已发布',
  failed: '失败',
  cancelled: '已取消',
  timed_out: '已超时',
  infrastructure_error: '基础设施错误',
}

export function evaluationStateLabel(value: string | null | undefined): string {
  return labelOf(EVALUATION_STATE, value)
}
