import { parseDocument, stringify } from 'yaml'
import type {
  PlatformConfigurationAppliedState,
  PlatformConfigurationApplyStage,
  PlatformConfigurationDesired,
  PlatformConfigurationFailureReason,
  PlatformConfigurationNextStep,
  UpdatePlatformConfigurationRequestSchema,
} from '@/generated/contracts'

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

export type AppliedTone = 'neutral' | 'ok' | 'warn'

export function appliedStateLabel(state: PlatformConfigurationAppliedState): { label: string; detail: string; tone: AppliedTone } {
  return ({
    unconfigured: { label: '尚无验证结果', detail: '还没有任何一次完整验证通过的应用。', tone: 'neutral' },
    verified: { label: '已验证', detail: '最近一次完整验证通过，applied 即当前生效配置。', tone: 'ok' },
    unverified: { label: '待核对', detail: '外部结果待核对；applied 只是最后一次验证通过的版本，不代表当前集群状态。', tone: 'warn' },
  } satisfies Record<PlatformConfigurationAppliedState, { label: string; detail: string; tone: AppliedTone }>)[state]
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

/**
 * Settings rejected by the Rust Control policy validator because no execution-side
 * consumer enforces them yet. The generated manifest test keeps this list aligned
 * with the Bundle and Helm boundaries.
 */
export interface UnsupportedPolicyField {
  path: string
  label: string
  /** The only value the deployment path accepts, as shown to the administrator. */
  accepted: string
}

export function unsupportedPolicyFields(configuration: PlatformConfigurationDesired): UnsupportedPolicyField[] {
  const runtime = configuration.runtimePolicy
  const builder = configuration.builderPolicy
  const fields: UnsupportedPolicyField[] = []
  if (runtime.defaultContainerUid !== 0) {
    fields.push({ path: 'runtimePolicy.defaultContainerUid', label: '默认容器 UID', accepted: '0' })
  }
  if (runtime.allowedCapabilities.length > 0) {
    fields.push({ path: 'runtimePolicy.allowedCapabilities', label: '普通容器 capabilities 允许清单', accepted: '空' })
  }
  if (runtime.preauthorizations.length > 0) {
    fields.push({ path: 'runtimePolicy.preauthorizations', label: '项目/课程预授权', accepted: '空' })
  }
  if (runtime.privilegedExecutionLocation?.kind === 'virtual_machine_guest') {
    fields.push({ path: 'runtimePolicy.privilegedExecutionLocation', label: 'VM guest 执行位置', accepted: '专用节点池或不允许特权' })
  }
  if (builder.capabilities.length > 0) {
    fields.push({ path: 'builderPolicy.capabilities', label: 'builder capabilities', accepted: '空' })
  }
  if (builder.allowPrivilegeEscalation) {
    fields.push({ path: 'builderPolicy.allowPrivilegeEscalation', label: 'builder 提权', accepted: '关闭' })
  }
  if (builder.entitlements.length > 0) {
    fields.push({ path: 'builderPolicy.entitlements', label: 'builder entitlements', accepted: '空' })
  }
  return fields
}

/** Copy of `configuration` with every unsupported setting reset to the accepted value; lifecycle and supported privileged placement are kept. */
export function withoutUnsupportedPolicy(configuration: PlatformConfigurationDesired): PlatformConfigurationDesired {
  const next = cloneConfiguration(configuration)
  next.runtimePolicy.defaultContainerUid = 0
  next.runtimePolicy.allowedCapabilities = []
  next.runtimePolicy.preauthorizations = []
  if (next.runtimePolicy.privilegedExecutionLocation?.kind === 'virtual_machine_guest') {
    next.runtimePolicy.privilegedExecutionLocation = null
    next.runtimePolicy.privilegedAllowed = false
  }
  next.builderPolicy = { capabilities: [], allowPrivilegeEscalation: false, entitlements: [] }
  return next
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
  const runtime = groups.runtimePolicy as Record<string, unknown>
  const builder = groups.builderPolicy as Record<string, unknown>
  const lists: [unknown, string][] = [
    [runtime.allowedCapabilities, 'runtimePolicy.allowedCapabilities'],
    [runtime.preauthorizations, 'runtimePolicy.preauthorizations'],
    [builder.capabilities, 'builderPolicy.capabilities'],
    [builder.entitlements, 'builderPolicy.entitlements'],
  ]
  for (const [list, path] of lists) {
    if (!Array.isArray(list)) return { ok: false, message: `${path} 必须是列表；更新会替换整个配置。` }
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
