import type { EnvironmentDetailSchema, EnvironmentOperationSnapshotSchema, SubmissionManifest } from '@/generated/contracts'
import { formatTimestamp } from '@/utils/format'
import { environmentOperationStateLabel, environmentStateLabel } from '@/utils/stateLabels'
import { makeDiagnostic } from '@/types/async'

export type EnvironmentLifecycleAction = 'start' | 'stop' | 'restart' | 'delete'
export type ConfirmedLifecycleAction = 'stop' | 'restart'
type OperationState = EnvironmentOperationSnapshotSchema['state']

/**
 * Neutral wording for usage and GPU capacity around stop. The platform does not
 * yet publish a per-environment stop impact, so the page must not promise that
 * charges stop or that a GPU reservation is kept or released.
 */
export const STOP_IMPACT_NOTICE = '提交停止不代表已停止计量：环境实际停止后，计算用量才停止计量；保留的工作目录和磁盘会继续计入存储用量，直到删除。GPU 容量是否保留取决于环境类型和项目资源配置，请以资源与费用记录为准。'
export const STOPPED_IMPACT_NOTICE = '保留的工作目录和磁盘会继续计入存储用量，直到删除。GPU 容量是否保留取决于环境类型和项目资源配置，请以资源与费用记录为准。'

export function isTerminalEnvironment(data: EnvironmentDetailSchema): boolean {
  return data.observedState === 'deleting' || data.observedState === 'deleted'
}

export function failedCleanup(data: EnvironmentDetailSchema): boolean {
  return data.desiredState === 'deleted'
    && data.observedState === 'failed'
    && data.operation.state === 'failed'
    && ['delete', 'expire', 'cleanup', 'cancel'].includes(data.operation.kind)
}

export function lifecycleConfirmationText(data: EnvironmentDetailSchema, action: ConfirmedLifecycleAction): string {
  const name = `“${data.displayLabel}”`
  const sessions = '会断开当前终端、桌面和 Web 访问会话，环境中正在运行的程序会结束，未保存到磁盘的内容会丢失。'
  if (action === 'restart') {
    return `重启${name}${sessions}环境随后会重新启动，重启完成前无法连接。工作目录和磁盘会保留。确定重启吗？`
  }
  // There is no stop-impact contract yet: billing and GPU capacity effects are
  // stated as conditional, never as something this action guarantees.
  return `停止${name}${sessions}工作目录和磁盘会保留，之后可以重新启动。${STOP_IMPACT_NOTICE}确定停止吗？`
}

export function deleteDialogCopy(data: EnvironmentDetailSchema | null) {
  const retry = Boolean(data && failedCleanup(data))
  return {
    title: retry ? '重试回收环境' : '删除环境',
    description: retry
      ? '上次回收失败，资源释放尚未确认。重试会继续回收该环境，并删除仍存在的工作目录及关联容器存储或虚拟机磁盘；操作不可恢复。项目材料、已冻结提交和评测记录不在本环境回收范围内。确定继续吗？'
      : '删除会回收整个环境，删除工作目录及关联容器存储或虚拟机磁盘；操作不可恢复。项目材料、已冻结提交和评测记录不在本环境删除范围内。确定继续吗？',
    confirmText: retry ? '重试回收' : '删除',
  }
}

export function desiredEnvironmentStateLabel(value: string): string {
  return ({
    running: '运行中',
    stopped: '已停止',
    deleted: '已删除',
  } as Record<string, string>)[value] ?? environmentStateLabel(value)
}

export function environmentMaterialDeadlineLabel(deadline: string | null): string {
  return deadline === null ? '不过期，直到明确撤回' : formatTimestamp(deadline)
}

/** The server reports the current instance generation ready; connection preparation is shown separately. */
export function canIssueAccessGrant(data: Pick<EnvironmentDetailSchema, 'desiredState' | 'connection'>): boolean {
  return data.desiredState === 'running' && data.connection.instanceReady
}

export function accessGrantBlockReason(data: EnvironmentDetailSchema): string {
  if (data.observedState === 'stopped') return '环境已停止，启动后才能签发访问授权。'
  if (data.observedState === 'failed') return '环境处于失败状态，重试成功并恢复就绪后才能签发访问授权。'
  if (isTerminalEnvironment(data)) return '环境正在删除或已删除，不能签发访问授权。'
  return '环境就绪后才能签发访问授权。'
}

export function operationStatusDescription(state: OperationState): string {
  switch (state) {
    case 'accepted':
      return '操作已受理，平台即将开始处理。'
    case 'running':
      return '操作正在处理。'
    case 'cancelling':
      return '正在取消操作并清理相关资源。'
    case 'succeeded':
      return '操作已完成。'
    case 'failed':
      return '操作未能完成。'
    case 'cancelled':
      return '操作已取消。'
    default:
      return '当前状态无法识别，请刷新页面。'
  }
}

export function operationTimelineDescription(op: EnvironmentOperationSnapshotSchema): string {
  const details = [operationStatusDescription(op.state), `第 ${op.attempt}/${op.maxAttempts} 次尝试`]
  if (op.diagnosticCode) {
    // The Chinese reason, when the code has one, comes before the code itself.
    const reason = makeDiagnostic(op.diagnosticCode, '').message
    if (reason) details.push(reason)
    details.push(`诊断码：${op.diagnosticCode}。`)
  }
  if (op.cleanupStartedAt) details.push(`资源清理已于 ${formatTimestamp(op.cleanupStartedAt)} 开始。`)
  return details.join(' ')
}

export function freezeStatusLabel(state: OperationState): string {
  switch (state) {
    case 'accepted':
      return '已排队'
    case 'running':
      return '冻结中'
    case 'cancelling':
      return '正在取消'
    case 'succeeded':
      return '冻结完成'
    case 'failed':
      return '冻结失败'
    case 'cancelled':
      return '已取消'
    default:
      return environmentOperationStateLabel(state)
  }
}

export function freezeStatusDescription(state: OperationState): string {
  switch (state) {
    case 'accepted':
      return '提交请求已进入队列，平台正在准备不可变快照。'
    case 'running':
      return '平台正在读取清单并保存不可变快照；此期间终端连接会暂时断开。'
    case 'cancelling':
      return '正在取消冻结并清理本次提交的处理中资源。'
    case 'succeeded':
      return '不可变提交快照已保存，评测结果会在结果页显示。'
    case 'failed':
      return '冻结提交未完成。请查看诊断码，确认环境仍就绪后再重试。'
    case 'cancelled':
      return '冻结提交已取消，尚未生成可供评测的不可变快照。'
    default:
      return operationStatusDescription(state)
  }
}

export function manifestRuleLabel(rule: SubmissionManifest['include'][number]): string {
  return rule.kind === 'directoryTree' ? `${rule.path}/（目录）` : rule.path
}
