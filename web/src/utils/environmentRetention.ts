import type {
  EnvironmentDetailSchema,
  EnvironmentDiskState,
  EnvironmentRuntimeAuthorizationResolution,
  RuntimeAdmissionBasis,
} from '@/generated/contracts'
import { makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import { formatTimestamp } from '@/utils/format'

type DiagnosticProblem = Parameters<typeof makeDiagnostic>[3]

/**
 * Read model for the Environment's independent disk fact (`diskRetention`) and
 * its approved runtime permissions. Everything here is derived from the server
 * detail; the page never infers a disk state from the compute state or a label.
 */

export type DiskRetention = NonNullable<EnvironmentDetailSchema['diskRetention']>

export const DISK_STATE_LABEL: Record<EnvironmentDiskState, string> = {
  retiring: '正在停止计算',
  retained: '磁盘保留中',
  restored: '磁盘已恢复',
  purging: '正在清除磁盘',
  purged: '磁盘已清除',
}

/**
 * Restore attaches the disk to a stopped generation only. Resource does not yet
 * offer funding for the restored generation (its fail-closed answer is
 * `LW_RESOURCE_USAGE_CONFLICT`), so the page never says restore re-funds compute.
 */
export const RESTORE_FUNDING_NOTICE = '恢复不会重新取得计算资金：删除时原资源批准的计算预占已释放，平台目前还没有为恢复后的环境提供新的资金授权，因此恢复后点击“启动”会被拒绝。'

function isVirtualMachine(data: Pick<EnvironmentDetailSchema, 'runtimeKind'>): boolean {
  return data.runtimeKind === 'virtual_machine'
}

/** The disk the retention window keeps, named the way the user sees it. */
export function retainedDiskName(data: Pick<EnvironmentDetailSchema, 'runtimeKind'>): string {
  return isVirtualMachine(data) ? '虚拟机系统盘' : '工作盘 /workspace'
}

/** What the retained disk keeps while compute is stopped. */
export function preservedItems(data: Pick<EnvironmentDetailSchema, 'runtimeKind'>): string[] {
  return isVirtualMachine(data)
    ? ['虚拟机系统盘及盘上的全部文件和已安装软件']
    : ['工作盘 /workspace 中的全部文件']
}

/** What is already gone once compute stopped, whatever happens to the disk. */
export function lostWithCompute(data: Pick<EnvironmentDetailSchema, 'runtimeKind'>): string[] {
  const items = ['正在运行的程序和内存中的内容', '终端、桌面、SSH 和 Web 应用连接及其访问授权']
  if (!isVirtualMachine(data)) items.push('容器中 /workspace 以外的改动（例如另行安装的软件、临时文件）')
  return items
}

/** What purge destroys for good. */
export function lostOnPurge(data: Pick<EnvironmentDetailSchema, 'runtimeKind'>): string[] {
  return isVirtualMachine(data)
    ? ['虚拟机系统盘，以及盘上的全部文件、已安装软件和系统配置']
    : ['工作盘 /workspace，以及其中的全部文件']
}

export const UNAFFECTED_BY_PURGE = '项目材料、已冻结的提交、评测成绩和资源与费用记录不在这块盘上，清除不影响它们。'

export function retentionExpired(disk: DiskRetention, now: number): boolean {
  return Date.parse(disk.expiresAt) <= now
}

/** Coarse remaining time; never a percentage or a countdown the server did not give. */
export function remainingText(expiresAt: string, now: number): string {
  const left = Date.parse(expiresAt) - now
  if (!Number.isFinite(left) || left <= 0) return '已到期'
  const hours = Math.floor(left / 3_600_000)
  if (hours < 1) return '剩余不到 1 小时'
  const days = Math.floor(hours / 24)
  if (days < 1) return `剩余约 ${hours} 小时`
  const rest = hours % 24
  return rest ? `剩余约 ${days} 天 ${rest} 小时` : `剩余约 ${days} 天`
}

/** One sentence on the compute side of the retained environment. */
export function computeStatusText(data: EnvironmentDetailSchema, disk: DiskRetention): string {
  const retiredAt = disk.computeRetiredAt ? `于 ${formatTimestamp(disk.computeRetiredAt)} ` : ''
  switch (disk.state) {
    case 'retiring':
      return isVirtualMachine(data)
        ? '正在停止计算：平台确认虚拟机实例已消失后，计算才算停止。'
        : '正在停止计算：平台确认运行实例和挂载这块盘的 Pod 全部消失后，计算才算停止。'
    case 'retained':
      return `计算已${retiredAt}停止（第 ${disk.retiredGeneration} 代），不再运行。`
    case 'restored':
      return disk.reactivatedAt
        ? `恢复后的环境已于 ${formatTimestamp(disk.reactivatedAt)} 重新运行。`
        : `磁盘已恢复到第 ${disk.restoredGeneration ?? '?'} 代，环境处于已停止状态，计算没有运行。`
    case 'purging':
      return `计算已${retiredAt}停止，平台正在永久清除磁盘。`
    case 'purged':
      return disk.purgedAt ? `磁盘已于 ${formatTimestamp(disk.purgedAt)} 永久清除，无法恢复。` : '磁盘已永久清除，无法恢复。'
  }
}

/** The deadline sentence for the state, from the immutable window the server fixed. */
export function deadlineText(disk: DiskRetention, now: number): string {
  const at = formatTimestamp(disk.expiresAt)
  switch (disk.state) {
    case 'retiring':
    case 'retained':
      return retentionExpired(disk, now)
        ? `七天保留期已于 ${at} 结束，磁盘等待平台清除，不能再恢复。`
        : `保留至 ${at}（${remainingText(disk.expiresAt, now)}）。到期后平台自动永久清除这块盘。`
    case 'restored':
      return disk.reactivatedAt
        ? '环境已重新运行，保留期结束。'
        : `恢复不延长保留期：如果在 ${at} 前没有重新运行，平台仍会按原期限清除这块盘。`
    case 'purging':
      return `保留期已于 ${at} 结束。`
    case 'purged':
      return `保留期已于 ${at} 结束。`
  }
}

export const PLATFORM_PAID_NOTICE = '删除后保留期间的存储费用由平台承担，不计入项目 Credits。'

/** A failed purge waits for the owner to continue it through `/recover`. */
export function failedPurge(data: EnvironmentDetailSchema): boolean {
  return data.desiredState === 'deleted'
    && data.diskRetention?.state === 'purging'
    && data.operation.kind === 'purge'
    && data.operation.state === 'failed'
}

export function restoreConfirmationText(data: EnvironmentDetailSchema): string {
  const disk = data.diskRetention
  const generation = data.generation + 1
  return [
    `恢复会把“${data.displayLabel}”的${retainedDiskName(data)}挂回一个新的运行代次（第 ${generation} 代），环境恢复为“已停止”，不会自动启动。`,
    '之前结束的程序、内存内容和访问授权不会恢复；恢复时会再次撤销旧的访问授权。',
    isVirtualMachine(data) ? '' : '容器中 /workspace 以外的改动在删除时已经丢失，启动后容器从模板镜像重新开始。',
    disk ? `恢复不会延长保留期：如果在 ${formatTimestamp(disk.expiresAt)} 前没有重新运行，平台仍会按原期限永久清除这块盘。` : '',
    RESTORE_FUNDING_NOTICE,
    '确定恢复吗？',
  ].filter(Boolean).join('')
}

export function purgeConfirmationText(data: EnvironmentDetailSchema): string {
  return [
    `保留期已经结束，上次清除“${data.displayLabel}”的磁盘没有完成。继续清除会永久删除：${lostOnPurge(data).join('；')}。`,
    '清除后无法恢复或找回。',
    UNAFFECTED_BY_PURGE,
    '确定继续清除吗？',
  ].join('')
}

// ---- Effective runtime permissions ----------------------------------------

export interface PermissionRow {
  key: 'user' | 'capabilities' | 'privileged' | 'boundary' | 'basis' | 'validUntil' | 'platform'
  label: string
  value: string
  /** The row grants more than the ordinary-container baseline. */
  elevated?: boolean
}

const BASIS_LABEL: Record<RuntimeAdmissionBasis, string> = {
  restricted_baseline: '平台基线（未使用额外预授权）',
  project_preauthorization: '项目预授权',
  course_preauthorization: '课程预授权',
}

/**
 * The template's declared run user, when the page can read it: `null` means the
 * declaration defers to the platform default, `undefined` that the declaration
 * is not available to the page.
 */
export type DeclaredRunAsUser = number | null | undefined

function uidText(uid: number): string {
  return uid === 0 ? 'UID 0（root）' : `UID ${uid}`
}

/** Rows for the runtime permissions the original Resource approval binds for this instance. */
export function effectivePermissionRows(
  data: Pick<EnvironmentDetailSchema, 'runtimeKind'>,
  authorization: EnvironmentRuntimeAuthorizationResolution,
  declaredRunAsUser: DeclaredRunAsUser,
): PermissionRow[] {
  const { permissions, basis, platformConfigurationRevision } = authorization.authorization
  const vm = isVirtualMachine(data)
  const rows: PermissionRow[] = []
  if (vm) {
    rows.push({ key: 'user', label: '运行用户', value: '使用虚拟机 guest 系统中的用户；容器运行用户设置不适用。' })
  } else if (declaredRunAsUser === undefined) {
    const uid = authorization.defaultContainerUid
    rows.push({
      key: 'user',
      label: '运行用户',
      value: `平台默认 ${uidText(uid)}；模板显式指定运行用户时以模板声明为准（本页没有读到模板声明）。`,
      elevated: uid === 0,
    })
  } else {
    const uid = declaredRunAsUser ?? authorization.defaultContainerUid
    rows.push({
      key: 'user',
      label: '运行用户',
      value: declaredRunAsUser === null ? `${uidText(uid)}，来自平台默认设置` : `${uidText(uid)}，由模板声明`,
      elevated: uid === 0,
    })
  }
  rows.push(vm
    ? { key: 'capabilities', label: '额外 Linux 能力', value: '不适用：虚拟机不使用容器能力设置。' }
    : permissions.capabilities.length > 0
      ? { key: 'capabilities', label: '额外 Linux 能力', value: `先丢弃全部能力，再只加入：${permissions.capabilities.join('、')}`, elevated: true }
      : { key: 'capabilities', label: '额外 Linux 能力', value: '无：全部丢弃，不加入任何能力' })
  rows.push(permissions.privileged
    ? { key: 'privileged', label: '特权模式', value: '已批准 privileged', elevated: true }
    : { key: 'privileged', label: '特权模式', value: '未启用（privileged=false）' })
  rows.push(permissions.privileged
    ? { key: 'boundary', label: '特权执行边界', value: '平台没有返回这个环境的执行边界状态；特权环境只能在管理员配置的专用边界运行，缺少边界时启动会被拒绝。', elevated: true }
    : { key: 'boundary', label: '特权执行边界', value: '不需要：非特权运行，不使用专用节点或独立虚拟机边界。' })
  rows.push({ key: 'basis', label: '授权依据', value: BASIS_LABEL[basis] ?? basis })
  rows.push({ key: 'validUntil', label: '授权有效至', value: formatTimestamp(authorization.validUntil) })
  rows.push({ key: 'platform', label: '平台配置版本', value: `rev-${platformConfigurationRevision}` })
  return rows
}

// ---- Readable diagnostics for retention and runtime permissions ------------

interface CodeText {
  reason: string
  nextStep?: string
  /** Repeating the same request cannot succeed. */
  final?: boolean
}

/**
 * Chinese reasons for the stable codes these pages surface. The Environment API
 * returns several of them with only a `diagnosticCode`; a server `detail`, when
 * present, still comes first.
 */
const CODE_TEXT: Record<string, CodeText> = {
  LW_ENVIRONMENT_DISK_NOT_RETAINED: { reason: '磁盘不在可恢复的保留状态；正在停止计算、正在清除或已经清除的磁盘不能恢复。', nextStep: '点击“刷新”查看磁盘当前状态。', final: true },
  LW_ENVIRONMENT_DISK_RETENTION_EXPIRED: { reason: '七天磁盘保留期已结束，不能恢复。', nextStep: '磁盘会由平台清除。需要环境时请提交新的资源申请。', final: true },
  LW_ENVIRONMENT_DISK_PURGE_NOT_DUE: { reason: '磁盘保留期尚未结束，不能清除。', final: true },
  LW_ENVIRONMENT_CLEANUP_PROTOCOL_UPGRADE_REQUIRED: { reason: '旧版本的清理操作已安全隔离，平台不会自动重新绑定或删除磁盘。', nextStep: '请把下方诊断信息发给管理员核对。', final: true },
  LW_RESOURCE_USAGE_CONFLICT: { reason: '恢复后的环境还不能重新运行：删除时原资源批准的计算预占已释放，平台目前还没有为恢复后的环境提供新的资金授权。', nextStep: '重试不会成功。磁盘仍按原期限保留；需要运行环境时请提交新的资源申请。', final: true },
  LW_RUNTIME_FUNDING_INACTIVE: { reason: '这个环境当前没有有效的资金覆盖，不能开始新的计算。', nextStep: '请查看项目 Credits 与资源申请；需要时提交新的资源申请。', final: true },
  LW_RUNTIME_FUNDING_CHANGED: { reason: '资源资金在核对期间发生了变化，本次操作没有执行。', nextStep: '请稍后重试同一操作。' },
  LW_RUNTIME_AUTHORITY_UNAVAILABLE: { reason: '平台暂时无法核对这个环境的运行授权，本次操作没有执行。', nextStep: '请稍后重试同一操作。' },
  LW_RUNTIME_AUTHORITY_READ_FAILED: { reason: '平台读取运行授权失败，本次操作没有执行。', nextStep: '请稍后重试；多次失败时把诊断信息发给管理员。' },
  LW_RUNTIME_SCOPE_DENIED: { reason: '你的项目或课程范围不包含这个环境的运行授权。', nextStep: '请确认所在项目和课程，或联系项目负责人。', final: true },
  LW_RUNTIME_TEACHER_APPROVAL_REQUIRED: { reason: '这个课程环境需要任课教师批准运行权限。', nextStep: '请联系任课教师批准后再试。', final: true },
  LW_RUNTIME_PERMISSION_OUTSIDE_PLATFORM_RANGE: { reason: '模板申请的运行权限（例如 privileged 或 NET_ADMIN 等能力）超出了平台允许的范围。', nextStep: '请改用平台允许的权限，或联系平台管理员调整允许范围。', final: true },
  LW_RUNTIME_PERMISSION_APPROVAL_REQUIRED: { reason: '模板申请的额外运行权限没有对应的项目或课程预授权。', nextStep: '请联系项目负责人或管理员申请相应预授权。', final: true },
  LW_RUNTIME_POLICY_NOT_VERIFIED: { reason: '平台运行配置还没有完成验证，暂时不能授予运行权限。', nextStep: '请稍后重试，或联系平台管理员。' },
  LW_RUNTIME_POLICY_CHANGED: { reason: '平台运行配置已更换，原批准中的运行权限不再适用。', nextStep: '请重新提交资源申请。', final: true },
  LW_RUNTIME_CONFIGURATION_BINDING_MISMATCH: { reason: '环境配置与原资源批准记录的配置不一致，平台拒绝运行。', nextStep: '请重新提交资源申请。', final: true },
  LW_RUNTIME_SECURITY_UNSUPPORTED: { reason: '模板的安全设置不受支持：提权、privileged、关闭 seccomp 或广泛内核能力当前都会被拒绝。', nextStep: '请修改模板的安全设置后重新发布。', final: true },
  LW_RUNTIME_USER_UNSUPPORTED: { reason: '模板指定的运行用户不受支持。', nextStep: '请修改模板的运行用户后重新发布。', final: true },
  LW_RUNTIME_RELEASE_UNAVAILABLE: { reason: '环境使用的模板版本当前不可用。', final: true },
  LW_RUNTIME_ALLOCATION_PATH_UNSUPPORTED: { reason: '这个旧环境没有资源租约，不能开始新的计算。', nextStep: '请提交新的资源申请。', final: true },
  LW_RUNTIME_EXECUTION_DEADLINE_EXCEEDED: { reason: '运行授权的有效期已过，本次操作没有执行。', final: true },
}

export function knownEnvironmentCode(code: string | null | undefined): boolean {
  return Boolean(code && CODE_TEXT[code])
}

/**
 * `/recover` admits only the environment owner. Its generic scope refusal
 * carries no detail, so restore and purge continuation name that rule; other
 * routes keep their own wording for the same code.
 */
const DISK_RECOVERY_CODE_TEXT: Record<string, CodeText> = {
  LW_AUTH_SCOPE_DENIED: { reason: '只有这个环境的所有者可以恢复磁盘或继续清除磁盘，平台拒绝了这次请求。', nextStep: '请联系环境所有者操作；重试不会成功。', final: true },
}

function diagnosticFrom(table: Record<string, CodeText>, code: string, detail: string | undefined, fallback: string, retryable: boolean, problem?: DiagnosticProblem): DiagnosticViewModel {
  const text = table[code]
  const diagnostic = makeDiagnostic(code, detail ?? text?.reason ?? fallback, text?.final ? false : retryable, problem)
  if (text?.nextStep) diagnostic.nextStep = text.nextStep
  return diagnostic
}

/**
 * Diagnostic for a rejected lifecycle or retention request: the server detail
 * or the readable Chinese reason first, then code/requestId via the banner.
 */
export function environmentDiagnostic(code: string, detail: string | undefined, fallback: string, retryable: boolean, problem?: DiagnosticProblem): DiagnosticViewModel {
  return diagnosticFrom(CODE_TEXT, code, detail, fallback, retryable, problem)
}

/** A refused `/recover` (restore or purge continuation); the server's refusal stays authoritative. */
export function diskRecoveryDiagnostic(code: string, detail: string | undefined, fallback: string, retryable: boolean, problem?: DiagnosticProblem): DiagnosticViewModel {
  return diagnosticFrom(DISK_RECOVERY_CODE_TEXT[code] ? DISK_RECOVERY_CODE_TEXT : CODE_TEXT, code, detail, fallback, retryable, problem)
}

/** A persisted operation code shown without a request: reason and next step only. */
export function persistedEnvironmentDiagnostic(code: string): DiagnosticViewModel {
  return environmentDiagnostic(code, undefined, '平台记录了这次操作的诊断码。', false)
}
