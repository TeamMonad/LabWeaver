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

      const pageWidth = await page.evaluate(() => ({
        clientWidth: document.documentElement.clientWidth,
        scrollWidth: document.documentElement.scrollWidth,
      }))
      expect(pageWidth.scrollWidth, 'shell must not create page-level horizontal overflow').toBeLessThanOrEqual(
        pageWidth.clientWidth,
      )

      if (bp.width >= 840) {
        const drawer = page.locator('.navigation-drawer')
        const drawerBox = await drawer.boundingBox()
        expect(drawerBox?.height, 'desktop drawer must occupy the content row').toBeGreaterThan(600)

        const drawerItems = page.locator('.drawer-item')
        await expect(drawerItems).toHaveCount(4)
        for (const item of await drawerItems.all()) {
          const itemBox = await item.boundingBox()
          expect(itemBox?.height, 'desktop drawer items must not collapse').toBeGreaterThanOrEqual(48)
        }
      }

      await expect(page).toHaveScreenshot(`home-${theme}-${bp.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}
