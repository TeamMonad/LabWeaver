import type {
  EnvironmentCandidateViewSchema,
  EnvironmentRuntimeAuthorizationResolution,
  LinuxCapability,
} from '@/generated/contracts'
import { networkLabel } from '@/utils/authoringReview'

type EnvironmentSpec = EnvironmentCandidateViewSchema['candidate']['spec']
type EnvironmentSecurity = EnvironmentSpec['security']

export type PermissionTone = 'restricted' | 'open' | 'neutral'
export type PermissionRow = { label: string; value: string; hint?: string; tone: PermissionTone }
export type PermissionGroup = { label: string; rows: PermissionRow[] }

function runAsUserRow(runAsUser: number | null | undefined): PermissionRow {
  if (runAsUser === null || runAsUser === undefined) {
    return {
      label: '运行用户',
      value: '平台默认 UID（容器默认 0；与特权模式独立）',
      hint: 'null 消费同一已验证平台默认 UID，不表示已获特权。',
      tone: 'neutral',
    }
  }
  if (runAsUser === 0) {
    return {
      label: '运行用户',
      value: 'UID 0（root 身份）',
      hint: '运行用户为 root 不等于 privileged；是否特权由 privileged 字段单独声明。',
      tone: 'open',
    }
  }
  return {
    label: '运行用户',
    value: `UID ${runAsUser}（非 root）`,
    tone: 'restricted',
  }
}

function rootFilesystemRow(security: EnvironmentSecurity, container: boolean): PermissionRow {
  const policy = security.rootFilesystemPolicy ?? 'mutable_required'
  if (policy === 'read_only_required') {
    return { label: '系统目录', value: '只读，只能写工作目录', tone: 'restricted' }
  }
  return {
    label: '系统目录',
    value: '可写',
    hint: container ? '容器重建后系统目录的修改不会保留；需要保留时保存为新镜像。' : undefined,
    tone: 'open',
  }
}

function escalationRow(security: EnvironmentSecurity): PermissionRow {
  const policy = security.privilegeEscalationPolicy ?? 'deny'
  return policy === 'deny'
    ? { label: '提升权限', value: '禁止（不能通过 sudo 等方式获得更高权限）', tone: 'restricted' }
    : { label: '提升权限', value: '允许', tone: 'open' }
}

function privilegedRow(security: EnvironmentSecurity): PermissionRow {
  const privileged = security.privileged ?? false
  return privileged
    ? {
        label: '特权模式',
        value: '已声明 privileged',
        hint: '实际 privileged / 更广内核能力仍由平台拒绝，除非已验证执行位置；nodeSelector 不构成充分证明。',
        tone: 'open',
      }
    : { label: '特权模式', value: '关闭（默认）', tone: 'restricted' }
}

function capabilitiesRow(security: EnvironmentSecurity): PermissionRow {
  const caps = security.capabilities ?? []
  if (caps.length === 0) {
    return { label: 'Capabilities', value: '无额外能力（默认 drop ALL）', tone: 'restricted' }
  }
  return {
    label: 'Capabilities',
    value: (caps as LinuxCapability[]).join(', '),
    hint: '能力集须落在平台允许 / 预授权范围内；超出由服务端拒绝。',
    tone: 'open',
  }
}

function seccompRow(security: EnvironmentSecurity): PermissionRow {
  const profile = security.seccomp ?? 'runtime_default'
  return profile === 'unconfined'
    ? {
        label: 'seccomp',
        value: 'unconfined',
        hint: '仅在 privileged 且平台显式允许时可用。',
        tone: 'open',
      }
    : { label: 'seccomp', value: 'runtime_default', tone: 'restricted' }
}

/**
 * Runtime permissions declared on an environment candidate / release spec.
 * Reads the #258 EnvironmentSpec.security surface (runAsUser / privileged /
 * capabilities / seccomp / rootFS / escalation); does not invent admin-scope
 * comparison that the candidate itself does not carry.
 */
export function runtimePermissionGroups(spec: EnvironmentSpec): PermissionGroup[] {
  const { security } = spec
  const container = spec.runtime.kind === 'container'
  const identity: PermissionRow[] = [
    runAsUserRow(security.runAsUser),
    rootFilesystemRow(security, container),
    escalationRow(security),
    privilegedRow(security),
    capabilitiesRow(security),
    seccompRow(security),
    { label: '安全配置', value: security.securityProfileBinding, hint: '由管理员策略定义', tone: 'neutral' },
  ]
  const access: PermissionRow[] = [
    { label: '网络', value: networkLabel(spec.network), tone: spec.network.mode === 'allow_all' ? 'open' : 'restricted' },
    security.publicExposurePolicy === 'deny'
      ? { label: '公网暴露', value: '禁止，只能通过平台入口访问', tone: 'restricted' }
      : { label: '公网暴露', value: String(security.publicExposurePolicy), tone: 'neutral' },
    ...spec.entries.map((entry): PermissionRow => ({
      label: '入口',
      value: `${entry.name} · ${entry.protocol === 'ssh' ? 'SSH' : entry.protocol.toUpperCase()} · 端口 ${entry.servicePort}`,
      tone: 'neutral',
    })),
  ]
  const resources: PermissionRow[] = spec.resources.gpu
    ? [{ label: 'GPU', value: `${spec.resources.gpu.class} × ${spec.resources.gpu.count}`, tone: 'open' }]
    : []
  return [
    { label: '运行身份与文件系统', rows: identity },
    { label: '网络与访问', rows: access },
    ...(resources.length ? [{ label: '设备', rows: resources }] : []),
  ]
}

/**
 * Receipt of the original ResourceApproval on an Environment instance.
 * Source is re-read before new compute; this is not a second grant.
 */
export function runtimeAuthorizationReceiptGroups(
  receipt: EnvironmentRuntimeAuthorizationResolution,
): PermissionGroup[] {
  const { authorization, leaseAuthorization } = receipt
  const caps = authorization.permissions.capabilities
  const scope =
    authorization.permissions.scope.kind === 'course'
      ? `课程 ${authorization.permissions.scope.courseId}`
      : `项目 ${authorization.permissions.scope.projectId}`
  const target =
    authorization.target.kind === 'environment'
      ? `环境 ${authorization.target.environmentId} · release ${authorization.target.releaseId}@${authorization.target.releaseVersion}`
      : `任务 ${authorization.target.taskRunId}`
  const location = receipt.privilegedExecutionLocation
    ? receipt.privilegedExecutionLocation.kind === 'dedicated_node_pool'
      ? `专用节点池（taint ${receipt.privilegedExecutionLocation.taintKey}=${receipt.privilegedExecutionLocation.taintValue}）`
      : `虚拟机客户机（${receipt.privilegedExecutionLocation.providerBinding}）`
    : '未声明（普通执行位置）'

  return [
    {
      label: '运行批准回执',
      rows: [
        { label: '资源请求', value: authorization.requestId, hint: `代次 ${authorization.requestGeneration}`, tone: 'neutral' },
        { label: '准入依据', value: authorization.basis, tone: 'neutral' },
        { label: '目标', value: target, tone: 'neutral' },
        { label: '预授权范围', value: scope, tone: 'neutral' },
        {
          label: '已批准特权',
          value: authorization.permissions.privileged ? 'privileged=true' : 'privileged=false',
          tone: authorization.permissions.privileged ? 'open' : 'restricted',
        },
        {
          label: '已批准能力',
          value: caps.length ? caps.join(', ') : '无额外能力',
          tone: caps.length ? 'open' : 'restricted',
        },
        {
          label: '平台配置',
          value: `revision ${authorization.platformConfigurationRevision}`,
          hint: authorization.platformConfigurationSha256,
          tone: 'neutral',
        },
        { label: '默认容器 UID', value: String(receipt.defaultContainerUid), tone: 'neutral' },
        { label: 'Provider', value: receipt.providerBinding, tone: 'neutral' },
        { label: '有效期至', value: receipt.validUntil, tone: 'neutral' },
        { label: '执行位置', value: location, hint: 'nodeSelector / 执行位置声明不构成已运行隔离的证明。', tone: 'neutral' },
        {
          label: 'Lease',
          value: leaseAuthorization.leaseId,
          hint: `${leaseAuthorization.activeFrom} → ${leaseAuthorization.expiresAt}`,
          tone: 'neutral',
        },
      ],
    },
  ]
}

/**
 * Residual #207 fields the candidate/spec path still cannot honestly answer
 * without platform-config comparison beyond the instance receipt.
 */
export const MISSING_PERMISSION_FIELDS = [
  '管理员策略允许的能力上限与本版本是否超出（候选规格不含对照结果；实例侧见运行批准回执）',
] as const

/** Experiment create without an original ResourceRequest→approval→lease handoff. */
export const EXPERIMENT_CREATE_UNSUPPORTED = {
  code: 'LW_RUNTIME_ALLOCATION_PATH_UNSUPPORTED',
  message:
    '实验环境创建尚未接通原 ResourceRequest→批准→lease→handoff 路径。已拨款的 Experiment lease 走旧 reservation 会返回 422；请等待统一资金/handoff 流程就绪后再创建，不要使用假的 resourceRequestId。',
  retryable: false,
} as const
