const MAX_DEMO_SLOW_MO_MS = 5_000

export function parseDemoSlowMo(raw) {
  if (raw === undefined || raw.trim() === '') return 0
  const value = Number(raw)
  if (!Number.isSafeInteger(value) || value < 0 || value > MAX_DEMO_SLOW_MO_MS) {
    throw new Error('PW_DEMO_SLOW_MO_INVALID')
  }
  return value
}
