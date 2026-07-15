import { test, expect } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'

const pages = ['/', '/not-found']

for (const path of pages) {
  test(`axe check ${path === '/' ? 'home' : path}`, async ({ page }) => {
    await page.goto(path)
    await page.waitForSelector('#app')
    const accessibilityScanResults = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa'])
      .analyze()
    expect(accessibilityScanResults.violations).toEqual([])
  })
}

test('keyboard: opening mobile drawer moves focus to close button', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/')
  await page.waitForSelector('.home-view')

  // First tab lands on the menu button in the top app bar.
  await page.keyboard.press('Tab')
  const activeBefore = await page.evaluate(() => document.activeElement?.getAttribute('aria-label'))
  expect(activeBefore).toBe('打开导航')

  // Activating the menu button opens the modal drawer.
  await page.keyboard.press('Enter')
  await page.waitForSelector('.navigation-drawer--open')

  // Focus should be moved to the drawer close button for accessibility.
  const activeAfter = await page.evaluate(() => document.activeElement?.getAttribute('aria-label'))
  expect(activeAfter).toBe('关闭导航')
})
