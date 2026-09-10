import { test, expect } from '@playwright/test'

const breakpoints = [
  { name: 'desktop', width: 1440, height: 900 },
  { name: 'tablet', width: 840, height: 900 },
  { name: 'mobile', width: 390, height: 844 },
]

const themes = ['light', 'dark']

for (const theme of themes) {
  for (const bp of breakpoints) {
    test(`home ${theme} ${bp.name} (${bp.width}x${bp.height})`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: bp.width, height: bp.height })
      await page.goto('/')
      await page.waitForSelector('.home-view')
      if (bp.name !== 'mobile') {
        // Desktop/tablet pin the drawer into the app-shell grid; assert the role
        // entries render so a stale golden cannot mask a broken sidebar.
        await expect(page.locator('.drawer-item')).toHaveCount(4)
        await expect(page.locator('.drawer-item').first()).toBeVisible()
      }
      if (bp.name === 'mobile') {
        const widths = await page.evaluate(() => ({
          viewport: document.documentElement.clientWidth,
          document: document.documentElement.scrollWidth,
          body: document.body.scrollWidth,
        }))
        expect(widths.document).toBeLessThanOrEqual(widths.viewport)
        expect(widths.body).toBeLessThanOrEqual(widths.viewport)
      }
      await expect(page).toHaveScreenshot(`home-${theme}-${bp.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}
