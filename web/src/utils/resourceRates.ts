import type { CreateResourceRateRequestSchema, GpuAllocationMode, ResourceBillingUnit, ResourceRateSchema } from '@/generated/contracts'

/**
 * Display metadata for billing units: `quantity` base units make one `label`
 * so administrators enter familiar prices (core-hour, GiB-hour, 1M tokens).
 */
export const rateDisplayUnits = {
  cpu_millicore_second: { base: 'CPU millicore 秒', label: '核心小时', quantity: 1000n * 3600n },
  memory_byte_second: { base: '内存字节秒', label: 'GiB 小时', quantity: 1024n ** 3n * 3600n },
  storage_byte_second: { base: '存储字节秒', label: 'GiB 小时', quantity: 1024n ** 3n * 3600n },
  gpu_unit_second: { base: 'GPU 分配单位秒', label: 'GPU 分配单位秒', quantity: 1n },
  build_second: { base: '构建秒', label: '构建小时', quantity: 3600n },
  llm_input_token: { base: '输入 token', label: '百万输入 token', quantity: 1_000_000n },
  llm_output_token: { base: '输出 token', label: '百万输出 token', quantity: 1_000_000n },
  llm_cache_read_token: { base: '缓存读取 token', label: '百万缓存读取 token', quantity: 1_000_000n },
  llm_cache_write_token: { base: '缓存写入 token', label: '百万缓存写入 token', quantity: 1_000_000n },
} as const satisfies Record<ResourceBillingUnit, { readonly base: string; readonly label: string; readonly quantity: bigint }>

/** The four model dimensions Resource requires before it admits one model call. */
export const modelTokenUnits = ['llm_input_token', 'llm_output_token', 'llm_cache_read_token', 'llm_cache_write_token'] as const satisfies readonly ResourceBillingUnit[]

export type ModelTokenUnit = typeof modelTokenUnits[number]

export const modelTokenUnitLabels: Record<ModelTokenUnit, string> = {
  llm_input_token: '输入',
  llm_output_token: '输出',
  llm_cache_read_token: '缓存读取',
  llm_cache_write_token: '缓存写入',
}

export function isModelTokenUnit(unit: string): unit is ModelTokenUnit {
  return (modelTokenUnits as readonly string[]).includes(unit)
}

/** Resource builder binding limit (bytes). */
export const BUILDER_BINDING_MAX_BYTES = 128
/** Resource model binding limit (bytes). */
export const MODEL_BINDING_MAX_BYTES = 256

const utf8 = new TextEncoder()

/**
 * Builder and model identities are opaque deployment identifiers. Same rule as
 * Resource: non-blank, at most `maxBytes` UTF-8 bytes, and no ASCII whitespace.
 */
export function validRateBinding(value: string, maxBytes: number): boolean {
  return value.trim().length > 0 && utf8.encode(value).length <= maxBytes && !/[\t\n\f\r ]/.test(value)
}

export type RateDisplayBillingUnit = keyof typeof rateDisplayUnits

/** Pad, never round, a user-entered price into the contract's exact six decimals. */
export function canonicalRateAmount(value: string): string | null {
  if (!/^(0|[1-9][0-9]*)(\.[0-9]{1,6})?$/.test(value)) return null
  const [whole, fraction = ''] = value.split('.')
  return `${whole}.${fraction.padEnd(6, '0')}`
}

/** A strictly positive exact CREDITS amount (per-call preauthorization cap), or null. */
export function canonicalPositiveCredits(value: string): string | null {
  const canonical = canonicalRateAmount(value.trim())
  return canonical && canonical !== '0.000000' ? canonical : null
}

function scaledAmount(canonical: string): bigint {
  return BigInt(canonical.replace('.', ''))
}

/** `datetime-local` value for a date in the browser's time zone (rate form default and reset). */
export function localDateTimeValue(date = new Date()): string {
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

export function toUtcTimestamp(value: string): string | null {
  const timestamp = Date.parse(value)
  return Number.isFinite(timestamp) ? new Date(timestamp).toISOString() : null
}

/**
 * Rate versions start in the future and end after they start, so a new
 * version never re-prices recorded usage. Returns a Chinese reason or null.
 */
export function rateWindowError(effectiveFrom: string, effectiveUntil: string, now: number): string | null {
  const from = Date.parse(effectiveFrom)
  if (!Number.isFinite(from)) return '请输入有效生效时间。'
  if (from <= now) return '新版本须在未来生效，不能回溯修改历史计价。'
  if (effectiveUntil && (!Number.isFinite(Date.parse(effectiveUntil)) || Date.parse(effectiveUntil) <= from)) return '结束时间须晚于生效时间。'
  return null
}

export function equivalentRatePrice(unit: ResourceBillingUnit, quantity: number, amount: string, currency: string): string | null {
  if (!Number.isSafeInteger(quantity) || quantity < 1 || !/^[A-Za-z0-9_-]{1,32}$/.test(currency)) return null
  const canonical = canonicalRateAmount(amount)
  const display = rateDisplayUnits[unit]
  if (!canonical || !display) return null
  const numerator = scaledAmount(canonical) * display.quantity
  const denominator = BigInt(quantity)
  let scaled = numerator / denominator
  const remainder = numerator % denominator
  if (remainder * 2n > denominator || (remainder * 2n === denominator && scaled % 2n === 1n)) scaled += 1n
  return `等价单价${remainder === 0n ? '' : '约'}：${scaled / 1_000_000n}.${(scaled % 1_000_000n).toString().padStart(6, '0')} ${currency} / ${display.label}（按六位小数显示）。`
}

export function rateVersionState(rate: ResourceRateSchema, now: number): string {
  const from = Date.parse(rate.effectiveFrom)
  const until = rate.effectiveUntil ? Date.parse(rate.effectiveUntil) : null
  if (!Number.isFinite(from) || (until !== null && !Number.isFinite(until))) return '时间无效'
  if (until !== null && until <= now) return '已结束'
  if (from > now) return '未来生效'
  return until !== null ? '已安排结束' : '现行'
}

/** Dimension fields of a rate draft, as they will be sent to Resource. */
export interface RateDimensionDraft {
  unit: ResourceBillingUnit
  gpuClass?: string | null
  gpuMode?: GpuAllocationMode | null
  builderBinding?: string | null
  modelBinding?: string | null
  maxPreauthorization?: string | null
}

/**
 * The dimension rule Resource enforces on rate creation
 * (LW_RESOURCE_RATE_DIMENSION_INVALID, and the matching rate contract check):
 * a builder binding exactly for build seconds, a GPU class and mode together
 * exactly for GPU units, and a model binding plus a positive per-call
 * preauthorization exactly for the four model token units. Returns the first
 * violation as a Chinese reason, or null.
 */
export function rateDimensionError(draft: RateDimensionDraft): string | null {
  const isBuild = draft.unit === 'build_second'
  const isGpu = draft.unit === 'gpu_unit_second'
  const isModel = isModelTokenUnit(draft.unit)
  const hasBuilder = draft.builderBinding != null
  const hasModel = draft.modelBinding != null
  const hasMax = draft.maxPreauthorization != null
  const hasClass = draft.gpuClass != null
  const hasMode = draft.gpuMode != null
  if (isBuild && !hasBuilder) return '构建费率必须填写构建器标识。'
  if (!isBuild && hasBuilder) return '只有构建费率可以填写构建器标识。'
  if (hasBuilder && !validRateBinding(draft.builderBinding!, BUILDER_BINDING_MAX_BYTES)) {
    return `构建器标识不能为空、不能含空格，且不超过 ${BUILDER_BINDING_MAX_BYTES} 字节。`
  }
  if (hasClass !== hasMode) return 'GPU 类型与分配模式必须同时选择。'
  if (isGpu && !hasClass) return 'GPU 费率必须选择 GPU 目录中的类型与分配模式。'
  if (!isGpu && hasClass) return '只有 GPU 费率可以选择 GPU 类型与分配模式。'
  if (isModel && !hasModel) return '模型 token 费率必须填写模型标识。'
  if (!isModel && (hasModel || hasMax)) return '只有模型 token 费率可以填写模型标识和单次预占上限。'
  if (hasModel && !validRateBinding(draft.modelBinding!, MODEL_BINDING_MAX_BYTES)) {
    return `模型标识不能为空、不能含空格，且不超过 ${MODEL_BINDING_MAX_BYTES} 字节。`
  }
  if (isModel && (!hasMax || canonicalPositiveCredits(draft.maxPreauthorization!) === null)) {
    return '模型 token 费率必须填写大于 0 的单次调用预占上限（最多六位小数）。'
  }
  return null
}

/**
 * Build and model admission only accept CREDITS rates: Resource rejects a
 * non-CREDITS model rate and refuses build estimates priced in another
 * currency. Other units may still carry a budget currency.
 */
export function rateCurrencyError(unit: ResourceBillingUnit, currency: string): string | null {
  if (!/^[A-Za-z0-9_-]{1,32}$/.test(currency)) return '币种只能包含字母、数字、下划线或连字符，最多 32 个字符。'
  if ((unit === 'build_second' || isModelTokenUnit(unit)) && currency !== 'CREDITS') {
    return '构建和模型费率只能使用 CREDITS，平台不做币种换算。'
  }
  return null
}

function sameDimension(rate: ResourceRateSchema, draft: RateDimensionDraft): boolean {
  return rate.unit === draft.unit
    && (rate.gpuClass ?? null) === (draft.gpuClass ?? null)
    && (rate.gpuMode ?? null) === (draft.gpuMode ?? null)
    && (rate.builderBinding ?? null) === (draft.builderBinding ?? null)
    && (rate.modelBinding ?? null) === (draft.modelBinding ?? null)
}

/**
 * The open version Resource closes when a new version of the same dimension
 * starts at `effectiveFrom`: the latest open version by start time, then
 * revision. Returns null when none exists.
 */
export function supersededRate(rates: readonly ResourceRateSchema[], draft: RateDimensionDraft): ResourceRateSchema | null {
  let latest: ResourceRateSchema | null = null
  for (const rate of rates) {
    if (rate.effectiveUntil || !sameDimension(rate, draft)) continue
    if (!latest) { latest = rate; continue }
    const order = Date.parse(rate.effectiveFrom) - Date.parse(latest.effectiveFrom)
    if (order > 0 || (order === 0 && rate.revision > latest.revision)) latest = rate
  }
  return latest
}

function effectiveAt(rate: ResourceRateSchema, at: number): boolean {
  const from = Date.parse(rate.effectiveFrom)
  const until = rate.effectiveUntil ? Date.parse(rate.effectiveUntil) : Number.POSITIVE_INFINITY
  return Number.isFinite(from) && from <= at && until > at
}

/** The rate Resource uses at `at` for one dimension: latest start, then highest revision. */
export function effectiveRate(rates: readonly ResourceRateSchema[], draft: RateDimensionDraft, at: number): ResourceRateSchema | null {
  let chosen: ResourceRateSchema | null = null
  for (const rate of rates) {
    if (!sameDimension(rate, draft) || !effectiveAt(rate, at)) continue
    if (!chosen) { chosen = rate; continue }
    const order = Date.parse(rate.effectiveFrom) - Date.parse(chosen.effectiveFrom)
    if (order > 0 || (order === 0 && rate.revision > chosen.revision)) chosen = rate
  }
  return chosen
}

export type ModelRateReadiness =
  | { kind: 'ready'; maxPreauthorization: string; rates: Record<ModelTokenUnit, ResourceRateSchema> }
  | { kind: 'missing'; missing: ModelTokenUnit[] }
  | { kind: 'cap_mismatch'; caps: string[] }

/**
 * Whether a model call could be admitted at `at` with the listed rates: all
 * four dimensions need an effective rate and every one must carry the same
 * per-call preauthorization cap. Resource refuses missing dimensions
 * (LW_RESOURCE_RATE_UNCONFIGURED) and differing caps
 * (LW_RESOURCE_RATE_CURRENCY_CONFLICT). This is a hint from the rate list;
 * the server decides at call time.
 */
export function modelRateReadiness(rates: readonly ResourceRateSchema[], modelBinding: string, at: number): ModelRateReadiness {
  const found: Partial<Record<ModelTokenUnit, ResourceRateSchema>> = {}
  const missing: ModelTokenUnit[] = []
  for (const unit of modelTokenUnits) {
    const rate = effectiveRate(rates, { unit, modelBinding }, at)
    if (rate) found[unit] = rate
    else missing.push(unit)
  }
  if (missing.length) return { kind: 'missing', missing }
  const caps = [...new Set(modelTokenUnits.map((unit) => found[unit]!.maxPreauthorization ?? ''))]
  if (caps.length !== 1 || !caps[0]) return { kind: 'cap_mismatch', caps: caps.filter(Boolean) }
  return { kind: 'ready', maxPreauthorization: caps[0], rates: found as Record<ModelTokenUnit, ResourceRateSchema> }
}

/** Model bindings that appear in the rate list, in first-seen order. */
export function modelBindingsOf(rates: readonly ResourceRateSchema[]): string[] {
  return [...new Set(rates.filter((rate) => isModelTokenUnit(rate.unit) && rate.modelBinding).map((rate) => rate.modelBinding!))]
}

/**
 * Tokens of one dimension a per-call cap covers at a display price
 * (CREDITS per million tokens), rounded down. Null for a zero price.
 */
export function tokensCoveredByCap(cap: string, pricePerMillion: string): bigint | null {
  const capCanonical = canonicalRateAmount(cap)
  const priceCanonical = canonicalRateAmount(pricePerMillion)
  if (!capCanonical || !priceCanonical) return null
  const price = scaledAmount(priceCanonical)
  if (price === 0n) return null
  return scaledAmount(capCanonical) * 1_000_000n / price
}

export interface ModelRateDraft {
  modelBinding: string
  maxPreauthorization: string
  prices: Record<ModelTokenUnit, string>
  effectiveFrom: string | null
  effectiveUntil: string | null
}

/**
 * The four create requests for one model, each at the shared cap and in
 * CREDITS per million tokens, or the first validation reason.
 */
export function modelRateRequests(draft: ModelRateDraft): { requests: Array<CreateResourceRateRequestSchema & { unit: ModelTokenUnit }> } | { error: string } {
  if (!draft.effectiveFrom) return { error: '请输入有效生效时间。' }
  const maxPreauthorization = canonicalPositiveCredits(draft.maxPreauthorization)
  const requests: Array<CreateResourceRateRequestSchema & { unit: ModelTokenUnit }> = []
  for (const unit of modelTokenUnits) {
    const amount = canonicalRateAmount(draft.prices[unit].trim())
    if (!amount) return { error: `请为${modelTokenUnitLabels[unit]} token 填写非负单价（最多六位小数）。` }
    const dimension = rateDimensionError({ unit, modelBinding: draft.modelBinding, maxPreauthorization: maxPreauthorization ?? draft.maxPreauthorization })
    if (dimension) return { error: dimension }
    requests.push({
      unit,
      unitQuantity: Number(rateDisplayUnits[unit].quantity),
      gpuClass: null,
      gpuMode: null,
      modelBinding: draft.modelBinding,
      maxPreauthorization: maxPreauthorization!,
      unitPrice: { currency: 'CREDITS', amount },
      effectiveFrom: draft.effectiveFrom,
      effectiveUntil: draft.effectiveUntil,
    })
  }
  return { requests }
}
