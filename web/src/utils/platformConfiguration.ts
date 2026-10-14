import { parseDocument, stringify } from 'yaml'
import type {
  PlatformConfigurationAppliedState,
  PlatformConfigurationApplyStage,
  PlatformConfigurationDesired,
  PlatformConfigurationFailureReason,
  PlatformConfigurationNextStep,
  PlatformConfigurationStatusSchemaBuilderEntitlement,
  PlatformConfigurationStatusSchemaLinuxCapability,
  UpdatePlatformConfigurationRequestSchema,
} from '@/generated/contracts'

export type LinuxCapability = PlatformConfigurationStatusSchemaLinuxCapability
export type BuilderEntitlement = PlatformConfigurationStatusSchemaBuilderEntitlement

/**
 * Runtime list of the generated `LinuxCapability` union. The generated SDK only
 * ships the type, so the exhaustiveness check below fails typecheck if the
 * contract adds or removes a capability.
 */
export const LINUX_CAPABILITIES = [
  'CHOWN', 'DAC_OVERRIDE', 'DAC_READ_SEARCH', 'FOWNER', 'FSETID', 'KILL', 'SETGID', 'SETUID', 'SETPCAP',
  'LINUX_IMMUTABLE', 'NET_BIND_SERVICE', 'NET_BROADCAST', 'NET_ADMIN', 'NET_RAW', 'IPC_LOCK', 'IPC_OWNER',
  'SYS_MODULE', 'SYS_RAWIO', 'SYS_CHROOT', 'SYS_PTRACE', 'SYS_PACCT', 'SYS_ADMIN', 'SYS_BOOT', 'SYS_NICE',
  'SYS_RESOURCE', 'SYS_TIME', 'SYS_TTY_CONFIG', 'MKNOD', 'LEASE', 'AUDIT_WRITE', 'AUDIT_CONTROL', 'SETFCAP',
  'MAC_OVERRIDE', 'MAC_ADMIN', 'SYSLOG', 'WAKE_ALARM', 'BLOCK_SUSPEND', 'AUDIT_READ', 'PERFMON', 'BPF',
  'CHECKPOINT_RESTORE',
] as const satisfies readonly LinuxCapability[]

export const BUILDER_ENTITLEMENTS = ['network.host', 'security.insecure'] as const satisfies readonly BuilderEntitlement[]

type Exhaustive<All, Listed> = [All] extends [Listed] ? true : never
const capabilitiesExhaustive: Exhaustive<LinuxCapability, (typeof LINUX_CAPABILITIES)[number]> = true
const entitlementsExhaustive: Exhaustive<BuilderEntitlement, (typeof BUILDER_ENTITLEMENTS)[number]> = true
void capabilitiesExhaustive
void entitlementsExhaustive

export const APPLY_STAGES = ['requested', 'validating', 'applying', 'verifying', 'succeeded'] as const satisfies readonly PlatformConfigurationApplyStage[]

export function applyStageLabel(stage: PlatformConfigurationApplyStage): string {
  return ({
    requested: '已记录请求',
    validating: '校验中',
    applying: '应用中',
    verifying: '验证中',
    succeeded: '已验证成功',
    failed: '失败',
  } satisfies Record<PlatformConfigurationApplyStage, string>)[stage]
}

export function appliedStateLabel(state: PlatformConfigurationAppliedState): { label: string; detail: string; tone: 'neutral' | 'ok' | 'warn' } {
  return ({
    unconfigured: { label: '尚无验证结果', detail: '还没有任何一次完整验证通过的应用。', tone: 'neutral' },
    verified: { label: '已验证', detail: '最近一次完整验证通过，applied 即当前生效配置。', tone: 'ok' },
    unverified: { label: '待核对', detail: '外部结果待核对；applied 只是最后一次验证通过的版本，不代表当前集群状态。', tone: 'warn' },
  } satisfies Record<PlatformConfigurationAppliedState, { label: string; detail: string; tone: 'neutral' | 'ok' | 'warn' }>)[state]
}

export function failureReasonLabel(reason: PlatformConfigurationFailureReason): string {
  return ({
    configuration_invalid: '配置无效',
    dependency_unavailable: '依赖不可用',
    execution_failed: '执行失败',
    verification_failed: '验证未通过',
    external_result_unknown: '外部结果未知',
    interrupted: '执行被中断',
  } satisfies Record<PlatformConfigurationFailureReason, string>)[reason]
}

export function nextStepLabel(step: PlatformConfigurationNextStep): string {
  return ({
    execute_approved_deployment: '等待既有获准部署入口执行此请求。浏览器不会触发部署。',
    wait_for_executor: '执行者正在处理，等待其报告下一阶段。',
    fix_configuration_and_retry: '修复配置或依赖，保存新的 desired 后再提交新的应用请求。',
    reconcile_external_result: '先核对外部真实状态，再继续获准执行。新的尝试不会清除待核对状态。',
    none: '无需进一步操作。',
  } satisfies Record<PlatformConfigurationNextStep, string>)[step]
}

const DAY = 86_400
const HOUR = 3_600
const MINUTE = 60

/** Human-readable hint for a seconds value; the API itself always stores seconds. */
export function formatSeconds(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return '—'
  if (seconds % DAY === 0) return `${seconds / DAY} 天`
  if (seconds % HOUR === 0) return `${seconds / HOUR} 小时`
  if (seconds % MINUTE === 0) return `${seconds / MINUTE} 分钟`
  return `${seconds} 秒`
}

export const SHA256_PATTERN = /^[0-9a-f]{64}$/

export function cloneConfiguration(configuration: PlatformConfigurationDesired): PlatformConfigurationDesired {
  return JSON.parse(JSON.stringify(configuration)) as PlatformConfigurationDesired
}

/** Stable JSON with sorted object keys, used for change detection and idempotency reuse. */
export function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`
  if (value && typeof value === 'object') {
    const entries = Object.entries(value as Record<string, unknown>)
      .filter(([, v]) => v !== undefined)
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonicalJson(v)}`).join(',')}}`
  }
  return JSON.stringify(value ?? null)
}

function flatten(value: unknown, prefix: string, out: Map<string, string>): void {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    const entries = Object.entries(value as Record<string, unknown>)
    if (entries.length === 0) out.set(prefix, '{}')
    for (const [key, child] of entries) flatten(child, prefix ? `${prefix}.${key}` : key, out)
    return
  }
  out.set(prefix, value === undefined ? 'null' : canonicalJson(value))
}

export interface ConfigurationFieldDifference {
  path: string
  left: string
  right: string
}

/** Field-level differences between two configurations (arrays compare as a whole). */
export function diffConfigurations(left: PlatformConfigurationDesired | null | undefined, right: PlatformConfigurationDesired | null | undefined): ConfigurationFieldDifference[] {
  const a = new Map<string, string>()
  const b = new Map<string, string>()
  if (left) flatten(left, '', a)
  if (right) flatten(right, '', b)
  const paths = [...new Set([...a.keys(), ...b.keys()])].sort()
  return paths
    .map((path) => ({ path, left: a.get(path) ?? '—', right: b.get(path) ?? '—' }))
    .filter((row) => row.left !== row.right)
}

export function requestToYaml(request: UpdatePlatformConfigurationRequestSchema): string {
  return stringify({ expectedRevision: request.expectedRevision, configuration: request.configuration }, { lineWidth: 0 })
}

export type YamlParseResult =
  | { ok: true; request: UpdatePlatformConfigurationRequestSchema }
  | { ok: false; message: string }

/**
 * Local pre-check of the advanced YAML. Duplicate keys and syntax errors are
 * reported here; field-level validation (unknown fields, ranges, capability
 * rules) stays with the server, which receives the YAML text unchanged.
 */
export function parseRequestYaml(text: string): YamlParseResult {
  const document = parseDocument(text, { uniqueKeys: true, prettyErrors: false })
  if (document.errors.length > 0) {
    const first = document.errors[0]
    const line = first.linePos?.[0]?.line
    return { ok: false, message: `YAML 无法解析${line ? `（第 ${line} 行）` : ''}：${first.code === 'DUPLICATE_KEY' ? '存在重复键' : '语法错误'}。` }
  }
  const value = document.toJS() as unknown
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return { ok: false, message: 'YAML 顶层必须是包含 expectedRevision 和 configuration 的对象。' }
  }
  const record = value as Record<string, unknown>
  if (typeof record.expectedRevision !== 'number' || !Number.isInteger(record.expectedRevision)) {
    return { ok: false, message: 'YAML 缺少整数 expectedRevision。' }
  }
  const configuration = record.configuration
  if (!configuration || typeof configuration !== 'object' || Array.isArray(configuration)) {
    return { ok: false, message: 'YAML 缺少 configuration 对象。' }
  }
  const groups = configuration as Record<string, unknown>
  for (const group of ['lifecycle', 'runtimePolicy', 'builderPolicy']) {
    if (!groups[group] || typeof groups[group] !== 'object') {
      return { ok: false, message: `configuration 缺少完整的 ${group} 设置组；更新会替换整个配置。` }
    }
  }
  return { ok: true, request: value as UpdatePlatformConfigurationRequestSchema }
}

/** Parse a comma/whitespace separated list of positive integer seconds. */
export function parseSecondsList(text: string): number[] | null {
  const parts = text.split(/[\s,，]+/).filter(Boolean)
  const values = parts.map((part) => Number(part))
  return values.every((value) => Number.isInteger(value) && value > 0) ? values : null
}

/** Parse `key=value` lines for a node selector. */
export function parseSelector(text: string): Record<string, string> | null {
  const result: Record<string, string> = {}
  for (const line of text.split('\n').map((l) => l.trim()).filter(Boolean)) {
    const index = line.indexOf('=')
    if (index <= 0) return null
    result[line.slice(0, index).trim()] = line.slice(index + 1).trim()
  }
  return result
}

export function formatSelector(selector: Record<string, string>): string {
  return Object.entries(selector).map(([key, value]) => `${key}=${value}`).join('\n')
}
