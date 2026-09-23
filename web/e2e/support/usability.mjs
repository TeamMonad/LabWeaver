import { expect, test } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'

/**
 * Indeterminate loading indicators the SPA renders while a request is in
 * flight: `AsyncStateView` renders `.spinner`, `DataTable` renders
 * `.skeleton-row`. A determinate `[role="progressbar"]` is deliberately absent
 * here so it is reported as fabricated progress instead of a stuck indicator.
 */
const LOADING_INDICATOR_SELECTOR = '.spinner, .skeleton-row, [aria-busy="true"]'
/**
 * Surfaces that would have to carry a server-reported ratio before a page may
 * display a percentage. The platform never reports determinate progress, so a
 * numeric value or `%` text on these elements is fabricated.
 */
const DETERMINATE_PROGRESS_SELECTOR = '[role="progressbar"], progress, [aria-valuenow], [aria-valuetext], .progress, .progress-bar'
const LOADING_SETTLE_TIMEOUT_MS = 30_000
const AXE_TAGS = Object.freeze(['wcag2a', 'wcag2aa'])
const BLOCKING_IMPACTS = Object.freeze(['serious', 'critical'])

/** Test metadata for artifact attachment; null outside a running test. */
function activeTestInfo() {
  try {
    return test.info()
  } catch {
    return null
  }
}

/**
 * Collect the two failure classes a user cannot act on: unhandled console
 * errors and uncaught page exceptions. Call `assertCleanConsole` at each
 * journey checkpoint so the failing surface is named.
 */
export function installUsabilityGuards(page) {
  const consoleErrors = []
  const pageErrors = []
  page.on('console', (message) => {
    if (message.type() === 'error') consoleErrors.push(message.text())
  })
  page.on('pageerror', (error) => {
    pageErrors.push(error.stack ?? error.message)
  })
  return Object.freeze({
    consoleErrors,
    pageErrors,
    assertCleanConsole(label) {
      const recorded = [
        ...consoleErrors.map((text) => `console: ${text}`),
        ...pageErrors.map((text) => `pageerror: ${text}`),
      ]
      if (recorded.length === 0) return
      throw new Error(`${label}:LW_ACCEPTANCE_CONSOLE_ERROR:${recorded.join(' | ')}`)
    },
  })
}

/**
 * Assert the surface settled: no loading indicator survives the bounded wait,
 * and no visible element claims a determinate progress value.
 */
export async function assertNoStuckProgress(page, label, { timeout = LOADING_SETTLE_TIMEOUT_MS } = {}) {
  const loading = page.locator(`${LOADING_INDICATOR_SELECTOR}:visible`)
  let stuck = []
  try {
    await expect
      .poll(
        async () => {
          stuck = await loading.evaluateAll((elements) => elements.map((element) => {
            const container = element.closest('.state-message') ?? element
            const identity = typeof element.className === 'string' && element.className ? element.className : element.tagName
            return `${identity}: ${(container.textContent ?? '').trim().slice(0, 120)}`
          }))
          return stuck.length
        },
        { timeout, intervals: [250, 500, 1000, 2000] },
      )
      .toBe(0)
  } catch (error) {
    throw new Error(
      `${label}:LW_ACCEPTANCE_STUCK_PROGRESS:${JSON.stringify(stuck)} after ${timeout}ms`,
      { cause: error },
    )
  }

  const fabricated = await page.locator(`${DETERMINATE_PROGRESS_SELECTOR}:visible`).evaluateAll((elements) => elements
    .map((element) => {
      const identity = typeof element.className === 'string' && element.className ? element.className : element.tagName
      return {
        element: identity,
        valueNow: element.getAttribute('aria-valuenow'),
        valueText: element.getAttribute('aria-valuetext'),
        value: element.hasAttribute('value') ? element.getAttribute('value') : null,
        text: (element.textContent ?? '').trim().slice(0, 120),
      }
    })
    .filter((item) => /\d+(?:[.,]\d+)?\s*%/.test(`${item.valueNow ?? ''} ${item.valueText ?? ''} ${item.value ?? ''} ${item.text}`)))
  if (fabricated.length > 0) {
    throw new Error(`${label}:LW_ACCEPTANCE_FABRICATED_PROGRESS:${JSON.stringify(fabricated)}`)
  }
}

/**
 * Run the WCAG A/AA axe scan. Serious and critical violations fail the journey;
 * every scan is attached to the test result so the remaining findings are
 * recorded with evidence instead of disappearing.
 */
export async function auditAccessibility(page, label, testInfo = null) {
  const results = await new AxeBuilder({ page }).withTags([...AXE_TAGS]).analyze()
  const info = testInfo ?? activeTestInfo()
  if (info) {
    await info.attach(`${label}-axe.json`, {
      body: JSON.stringify({
        label,
        url: page.url(),
        blockingImpacts: [...BLOCKING_IMPACTS],
        tags: [...AXE_TAGS],
        violations: results.violations,
        incomplete: results.incomplete,
        passCount: results.passes.length,
      }, null, 2),
      contentType: 'application/json',
    })
  }
  const blocking = results.violations.filter((violation) => BLOCKING_IMPACTS.includes(violation.impact))
  if (blocking.length > 0) {
    throw new Error(`${label}:LW_ACCEPTANCE_A11Y_SERIOUS:${blocking.map((violation) => `${violation.impact}:${violation.id} (${violation.nodes.length} node(s): ${violation.nodes
      .slice(0, 3)
      .map((node) => node.target.join(' '))
      .join(', ')})`).join(' | ')}`)
  }
  return results
}
