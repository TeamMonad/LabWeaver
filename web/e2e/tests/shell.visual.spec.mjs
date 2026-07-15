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
      await expect(page).toHaveScreenshot(`home-${theme}-${bp.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}
