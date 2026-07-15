/**
 * Fixture 固定时钟。
 * 所有时间相关数据都基于这个固定基准，确保截图、UUID、hash 可复现。
 */

const EPOCH = new Date('2026-07-11T10:00:00.000Z')

export function now(): Date {
  return new Date(EPOCH.getTime())
}

export function nowIso(): string {
  return EPOCH.toISOString()
}

export function addSeconds(seconds: number): Date {
  return new Date(EPOCH.getTime() + seconds * 1000)
}

export function addSecondsIso(seconds: number): string {
  return addSeconds(seconds).toISOString()
}

export function addHours(hours: number): Date {
  return new Date(EPOCH.getTime() + hours * 60 * 60 * 1000)
}

export function addHoursIso(hours: number): string {
  return addHours(hours).toISOString()
}
