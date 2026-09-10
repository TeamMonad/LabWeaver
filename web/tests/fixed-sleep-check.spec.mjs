import { describe, expect, it } from 'vitest'
import { isFixedSleepLine } from '../scripts/fixed-sleep-check.mjs'

describe('fixed sleep checker', () => {
  it.each([
    'setTimeout(resolve, 1000)',
    'globalThis.setTimeout(resolve, 1000)',
    'window . setTimeout(resolve, 1000)',
    'await page.waitForTimeout(1000)',
    'sleep(1)',
  ])('rejects fixed-delay API: %s', (line) => {
    expect(isFixedSleepLine(line)).toBe(true)
  })

  it('allows Playwright test deadlines', () => {
    expect(isFixedSleepLine('test.setTimeout(FULL_CHAIN_TIMEOUT_MS)')).toBe(false)
  })
})
