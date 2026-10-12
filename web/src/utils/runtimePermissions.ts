import type { EnvironmentCandidateViewSchema } from '@/generated/contracts'
import { networkLabel } from '@/utils/authoringReview'

type EnvironmentSpec = EnvironmentCandidateViewSchema['candidate']['spec']

export type PermissionTone = 'restricted' | 'open' | 'neutral'
export type PermissionRow = { label: string; value: string; hint?: string; tone: PermissionTone }
export type PermissionGroup = { label: string; rows: PermissionRow[] }

/**
 * Runtime permissions declared on an environment candidate, grouped the way a
 * reviewer reads them. Only fields present in EnvironmentSpec are shown; the
 * fields #207 adds (privileged mode, capabilities, seccomp, admin scope) are
 * listed separately as not yet provided.
 */
export function runtimePermissionGroups(spec: EnvironmentSpec): PermissionGroup[] {
  const { security } = spec
  const container = spec.runtime.kind === 'container'
  const readOnlyRoot = security.rootFilesystemPolicy === 'read_only_required'
  const identity: PermissionRow[] = [
    security.userPolicy === 'non_root_required'
      ? { label: '运行用户', value: '普通用户（不能使用 root）', tone: 'restricted' }
      : { label: '运行用户', value: String(security.userPolicy), tone: 'neutral' },
    readOnlyRoot
      ? { label: '系统目录', value: '只读，只能写工作目录', tone: 'restricted' }
      : {
          label: '系统目录',
          value: '可写',
          hint: container ? '容器重建后系统目录的修改不会保留；需要保留时保存为新镜像。' : undefined,
          tone: 'open',
        },
    security.privilegeEscalationPolicy === 'deny'
      ? { label: '提升权限', value: '禁止（不能通过 sudo 等方式获得更高权限）', tone: 'restricted' }
      : { label: '提升权限', value: String(security.privilegeEscalationPolicy), tone: 'neutral' },
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

/** Fields #207 introduces that the current contract does not carry. */
export const MISSING_PERMISSION_FIELDS = [
  '特权模式（privileged）是否开启',
  '额外的 capabilities（例如 NET_ADMIN）及各自用途',
  'seccomp 配置',
  '管理员策略允许的范围，以及本版本是否超出',
] as const
