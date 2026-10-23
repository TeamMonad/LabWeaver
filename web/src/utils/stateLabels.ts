import type { ProjectObjectRole } from '@/generated/contracts'

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
  awaiting_approval: '等待批准',
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
  awaiting_approval: '等待批准',
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
  work_configuration: 'Work 配置轨道',
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

const ENVIRONMENT_OPERATION_KIND: Record<string, string> = {
  create: '创建环境',
  start: '启动环境',
  stop: '停止环境',
  restart: '重启环境',
  reset: '重置环境',
  retry: '重试环境',
  cancel: '取消操作',
  recover: '恢复环境',
  expire: '处理过期环境',
  delete: '删除环境',
  cleanup: '清理环境',
  freeze: '冻结提交',
}

export function environmentOperationKindLabel(value: string | null | undefined): string {
  return value ? ENVIRONMENT_OPERATION_KIND[value] ?? '环境操作' : '环境操作'
}

const ENVIRONMENT_OPERATION_STATE: Record<string, string> = {
  accepted: '已受理',
  running: '处理中',
  cancelling: '取消中',
  succeeded: '已完成',
  failed: '失败',
  cancelled: '已取消',
}

export function environmentOperationStateLabel(value: string | null | undefined): string {
  return value ? ENVIRONMENT_OPERATION_STATE[value] ?? '状态未知' : '状态未知'
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

export function resourceAllocationFailureMessage(diagnosticCode: string | null | undefined): string | null {
  return diagnosticCode === 'LW_RESOURCE_WORK_ALLOCATION_BLOCKED'
    ? '分配失败，请回收后重新申请。'
    : null
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

const PROJECT_ROLE: Record<string, string> = {
  student: '学生',
  teacher: '教师',
  platform_admin: '平台管理员',
}

export function projectRoleLabel(value: string | null | undefined): string {
  return labelOf(PROJECT_ROLE, value)
}

/**
 * Project responsibility (`objectRole`), independent of the teaching identity
 * above. Keyed by the generated `ProjectObjectRole` union, so a role added to
 * the contract fails the type check until it is labelled and described here.
 */
const PROJECT_OBJECT_ROLE: Record<ProjectObjectRole, string> = {
  viewer: '查看者',
  operator: '操作者',
  editor: '编辑者',
  manager: '管理者',
  owner: '负责人',
}

export function projectObjectRoleLabel(value: string | null | undefined): string {
  return labelOf(PROJECT_OBJECT_ROLE, value)
}

/** What each responsibility permits, matching Control's `ProjectObjectRole::actions`. */
const PROJECT_OBJECT_ROLE_DESCRIPTION: Record<ProjectObjectRole, string> = {
  viewer: '只能查看项目、成员和环境。',
  operator: '可以查看，并启动、停止和重启项目环境。',
  editor: '可以编辑项目、新建 Work、创建和删除环境、签发访问授权、提交评测、配置 Agent 和查看费用；不能管理成员或归档项目。',
  manager: '拥有编辑者的全部权限，并可以管理成员；不能归档项目。',
  owner: '项目负责人，拥有管理者的全部权限并可以归档项目；创建项目时确定，不能通过成员管理授予。',
}

export function projectObjectRoleDescription(value: string | null | undefined): string {
  return labelOf(PROJECT_OBJECT_ROLE_DESCRIPTION, value)
}

export type GrantableProjectObjectRole = Exclude<ProjectObjectRole, 'owner'>

/** Responsibilities the member API can grant: every contract role except `owner`, which Control sets at project creation. */
export const GRANTABLE_PROJECT_OBJECT_ROLES = (Object.keys(PROJECT_OBJECT_ROLE) as ProjectObjectRole[])
  .filter((role): role is GrantableProjectObjectRole => role !== 'owner')

const MEMBERSHIP_STATE: Record<string, string> = {
  active: '生效中',
  suspended: '已暂停',
  revoked: '已撤销',
}

export function membershipStateLabel(value: string | null | undefined): string {
  return labelOf(MEMBERSHIP_STATE, value)
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
