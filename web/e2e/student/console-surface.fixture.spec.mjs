import { test, expect } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'

const viewports = [
  { name: 'desktop', width: 1440, height: 900 },
  { name: 'mobile', width: 390, height: 844 },
]

const themes = ['light', 'dark']

async function expectNoA11yViolations(page, message) {
  const axeResult = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze()
  expect(axeResult.violations, message).toEqual([])
}

async function createEnvAndGrant(page, runtimeLabel) {
  await page.goto('/student/environments')
  await page.waitForSelector('.environment-entry')
  await page.locator(`tr:has-text("${runtimeLabel}") button:has-text("创建环境")`).first().click()
  await expect(page.locator('.env-state')).toHaveText('ready', { timeout: 15000 })
  await page.locator('button:has-text("签发访问授权")').click()
  await expect(page.locator('.grant-card')).toBeVisible()
}

test('container xterm console opens, shows terminal, and disconnects on grant revoke', async ({ page }) => {
  await createEnvAndGrant(page, '容器')

  const openButton = page.locator('button:has-text("打开终端")')
  await expect(openButton).toBeEnabled()
  await openButton.click()

  // The fixture console opens a deterministic in-memory terminal.
  await expect(page.locator('.xterm-host')).toBeVisible({ timeout: 10000 })
  await expect(page.locator('.xterm-host')).toContainText('LabWeaver fixture console', { timeout: 10000 })

  await expectNoA11yViolations(page, 'xterm console should have no a11y violations')

  // Revoking the grant closes the console (fail closed).
  await page.locator('button:has-text("撤销授权")').click()
  await expect(page.locator('.xterm-host')).toHaveCount(0)
})

test('vm novnc console surfaces honest upstream-unavailable instead of a fake stream', async ({ page }) => {
  await createEnvAndGrant(page, '虚拟机')

  await page.locator('button:has-text("打开图形控制台")').click()

  // noVNC cannot fake the RFB protocol; the connection times out and the UI
  // shows the stable upstream-unavailable diagnostic with a retry affordance.
  await expect(page.locator('text=CONSOLE_UPSTREAM_UNAVAILABLE')).toBeVisible({ timeout: 15000 })

  await expectNoA11yViolations(page, 'novnc error state should have no a11y violations')
})

test('revoked grant denies console capability discovery', async ({ page }) => {
  await createEnvAndGrant(page, '容器')
  await page.locator('button:has-text("撤销授权")').click()
  await expect(page.locator('text=ACCESS_GRANT_REVOKED')).toBeVisible()
  await expect(page.locator('button:has-text("打开终端")')).toHaveCount(0)
})

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`xterm console ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await createEnvAndGrant(page, '容器')
      await page.locator('button:has-text("打开终端")').click()
      await expect(page.locator('.xterm-host')).toContainText('LabWeaver fixture console', { timeout: 10000 })

      await expect(page.locator('.console-panel')).toHaveScreenshot(`xterm-console-${theme}-${viewport.name}.png`, {
        animations: 'disabled',
      })
    })
  }
}
