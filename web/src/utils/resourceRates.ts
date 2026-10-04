import type { ResourceBillingUnit, ResourceRateSchema } from '@/generated/contracts'

export const rateDisplayUnits: Record<ResourceBillingUnit, { base: string; label: string; quantity: bigint }> = {
  cpu_millicore_second: { base: 'CPU millicore 秒', label: '核心小时', quantity: 1000n * 3600n },
  memory_byte_second: { base: '内存字节秒', label: 'GiB 小时', quantity: 1024n ** 3n * 3600n },
  storage_byte_second: { base: '存储字节秒', label: 'GiB 小时', quantity: 1024n ** 3n * 3600n },
  gpu_unit_second: { base: 'GPU 分配单位秒', label: 'GPU 分配单位秒', quantity: 1n },
}

/** Pad, never round, a user-entered price into the contract's exact six decimals. */
export function canonicalRateAmount(value: string): string | null {
  if (!/^(0|[1-9][0-9]*)(\.[0-9]{1,6})?$/.test(value)) return null
  const [whole, fraction = ''] = value.split('.')
  return `${whole}.${fraction.padEnd(6, '0')}`
}

export function equivalentRatePrice(unit: ResourceBillingUnit, quantity: number, amount: string, currency: string): string | null {
  if (!Number.isSafeInteger(quantity) || quantity < 1 || !/^[A-Za-z0-9_-]{1,32}$/.test(currency)) return null
  const canonical = canonicalRateAmount(amount)
  const display = rateDisplayUnits[unit]
  if (!canonical || !display) return null
  const numerator = BigInt(canonical.replace('.', '')) * display.quantity
  const denominator = BigInt(quantity)
  let scaled = numerator / denominator
  const remainder = numerator % denominator
  if (remainder * 2n > denominator || (remainder * 2n === denominator && scaled % 2n === 1n)) scaled += 1n
  const formatted = `${scaled / 1_000_000n}.${(scaled % 1_000_000n).toString().padStart(6, '0')}`
  return `等价单价${remainder === 0n ? '' : '约'}：${formatted} ${currency} / ${display.label}（按六位小数显示）。`
}

export function rateVersionState(rate: ResourceRateSchema, now: number): string {
  const from = Date.parse(rate.effectiveFrom)
  const until = rate.effectiveUntil ? Date.parse(rate.effectiveUntil) : null
  if (!Number.isFinite(from) || (until !== null && !Number.isFinite(until))) return '时间无效'
  if (until !== null && until <= now) return '已结束'
  return from > now ? '未来生效' : '现行'
}
