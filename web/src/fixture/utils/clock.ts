/**
 * Fixture 固定时钟。
 * 所有时间相关数据都基于这个固定基准，确保截图、UUID、hash 可复现。
 */

export const EPOCH = new Date('2026-07-11T10:00:00.000Z')

let clockOffsetSeconds = 0

export function now(): Date {
  return new Date(EPOCH.getTime() + clockOffsetSeconds * 1000)
}

export function nowIso(): string {
  return now().toISOString()
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

/**
 * Advance the fixture clock by the given number of seconds.
 *
 * This is a mutable, test-only utility; `resetFixtureState()` restores the
 * clock offset to zero.
 */
export function addClockSeconds(seconds: number): void {
  clockOffsetSeconds += seconds
}

export function resetClock(): void {
  clockOffsetSeconds = 0
}
