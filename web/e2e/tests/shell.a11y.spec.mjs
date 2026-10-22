import { test, expect } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'
import { fixtureUrl } from './fixture-shell.mjs'

const pages = [
  { name: 'home', path: '/', ready: '.home-view' },
  { name: '/not-found', path: '/not-found', ready: '#app main' },
]

for (const { name, path, ready } of pages) {
  test(`axe check ${name}`, async ({ page }) => {
    await page.goto(fixtureUrl(path))
    await page.waitForSelector(ready)
    const accessibilityScanResults = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa'])
      .analyze()
    expect(accessibilityScanResults.violations).toEqual([])
  })
}

test('keyboard: opening mobile drawer moves focus to close button', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(fixtureUrl('/'))
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

  // Escape closes the drawer and returns focus to the menu button.
  await page.keyboard.press('Escape')
  await expect(page.locator('.navigation-drawer--modal')).toHaveCount(0)
  const activeClosed = await page.evaluate(() => document.activeElement?.getAttribute('aria-label'))
  expect(activeClosed).toBe('打开导航')
})
