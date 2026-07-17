import { test, expect } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'

const viewports = [
  { name: 'desktop', width: 1440, height: 900 },
  { name: 'mobile', width: 390, height: 844 },
]

const themes = ['light', 'dark']

const fixturePublicKey = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJDHHUCBhBrVzOCaYFFl/wdnaJM8j2d3bohil0VRm78D fixture@labweaver.io'

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`ssh keys list ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/student/ssh-keys')
      await page.waitForSelector('.ssh-keys')
      await expect(page.locator('text=已登记公钥')).toBeVisible()
      await expect(page.locator('.data-table__row')).toHaveCount(2)

      const axeResult = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze()
      expect(axeResult.violations, 'list view should have no a11y violations').toEqual([])

      await expect(page).toHaveScreenshot(`ssh-keys-list-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`ssh keys add ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/student/ssh-keys')
      await page.waitForSelector('.ssh-keys')

      await page.locator('textarea[aria-label="OpenSSH 公钥"]').fill(fixturePublicKey)
      await page.locator('button:has-text("添加")').first().click()
      await expect(page.locator('.data-table__row')).toHaveCount(3)

      const axeResult = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze()
      expect(axeResult.violations, 'add flow should have no a11y violations').toEqual([])

      await expect(page).toHaveScreenshot(`ssh-keys-add-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}

test('ssh keys delete with confirmation', async ({ page }) => {
  await page.goto('/student/ssh-keys')
  await page.waitForSelector('.ssh-keys')
  await expect(page.locator('.data-table__row')).toHaveCount(2)

  // The delete button is an icon-only button with aria-label="删除".
  await page.locator('button[aria-label="删除"]').first().click()
  await expect(page.locator('text=确定删除')).toBeVisible()

  await page.locator('button:has-text("删除")').filter({ hasText: /^删除$/ }).click()
  await expect(page.locator('.data-table__row')).toHaveCount(1)
})
