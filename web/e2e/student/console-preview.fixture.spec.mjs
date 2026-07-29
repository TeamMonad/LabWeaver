import { test, expect } from '@playwright/test'

const viewports = [
  { name: 'desktop', width: 1440, height: 900 },
  { name: 'mobile', width: 390, height: 844 },
]

const themes = ['light', 'dark']

test.describe('fixture console preview route', () => {
  test.use({ storageState: 'e2e/fixtures/empty.json' })

  test('opens directly to xterm/noVNC layouts without any backend call', async ({ page }) => {
    const apiCalls = []
    page.on('request', (req) => {
      if (req.url().includes('/api/')) apiCalls.push(req.url())
    })

    await page.goto('/fixture/console-preview')
    await expect(page.locator('.console-preview')).toBeVisible()

    // xterm layout renders with the deterministic in-memory terminal.
    await expect(page.locator('.xterm-host')).toBeVisible({ timeout: 10000 })
    await expect(page.locator('.xterm-host')).toContainText('LabWeaver fixture console', { timeout: 10000 })

    // noVNC layout shows the honest upstream-unavailable state, not a fake frame.
    await expect(page.locator('.novnc-canvas')).toBeVisible()
    await expect(page.locator('text=CONSOLE_UPSTREAM_UNAVAILABLE')).toBeVisible()

    // No environment is created, no grant is issued, and no backend is called.
    expect(apiCalls, `expected no backend /api calls, got: ${apiCalls.join(', ')}`).toEqual([])
  })

  for (const theme of themes) {
    for (const viewport of viewports) {
      test(`preview xterm geometry ${theme} ${viewport.name}`, async ({ page }) => {
        await page.emulateMedia({ colorScheme: theme })
        await page.setViewportSize({ width: viewport.width, height: viewport.height })
        await page.goto('/fixture/console-preview')
        await expect(page.locator('.xterm-host')).toBeVisible({ timeout: 10000 })

        const consoleEl = page.locator('.xterm-console').first()
        const box = await consoleEl.boundingBox()
        expect(box).not.toBeNull()
        // Deterministic responsive geometry: desktop 400px, mobile 360px.
        const expectedHeight = viewport.name === 'mobile' ? 360 : 400
        expect(Math.round(box.height)).toBe(expectedHeight)

        await expect(consoleEl).toHaveScreenshot(`console-preview-xterm-${theme}-${viewport.name}.png`, {
          animations: 'disabled',
        })
      })
    }
  }
})
