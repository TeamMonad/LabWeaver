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
