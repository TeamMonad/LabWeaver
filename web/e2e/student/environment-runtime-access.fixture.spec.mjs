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

async function createEnvironmentFromRelease(page, runtimeLabel) {
  await page.goto('/student/environments')
  await page.waitForSelector('.environment-entry')
  await page.locator(`tr:has-text("${runtimeLabel}") button:has-text("创建环境")`).first().click()
  await expect(page).toHaveURL(/environmentId=/)
  await expect(page.locator('.env-state')).toHaveText('ready', { timeout: 15000 })
}

async function issueGrant(page) {
  await page.locator('button:has-text("签发访问授权")').click()
  await expect(page.locator('.grant-card')).toBeVisible()
}

test('student VM single-line SSH command, freeze evidence, and operations timeline', async ({ page }) => {
  await createEnvironmentFromRelease(page, '虚拟机')
  await issueGrant(page)

  // VM single-line SSH command with alias and gateway; no download config.
  const sshCommand = page.locator('.ssh-command__text')
  await expect(sshCommand).toContainText(/^ssh lw-[a-f0-9]{20}@gateway\.labweaver\.local$/)
  await expect(page.locator('text=Gateway fingerprint')).toBeVisible()
  await expect(page.locator('text=Grant：')).toBeVisible()

  await expectNoA11yViolations(page, 'ssh access card should have no a11y violations')

  // Freeze submission and inspect evidence identity.
  await page.locator('button:has-text("冻结提交")').click()
  await expect(page.getByText('Object Version', { exact: true })).toBeVisible({ timeout: 15000 })
  await expect(page.getByText('SHA-256', { exact: true })).toBeVisible()
  await expect(page.locator('.evidence-card')).toContainText('sha256:')

  // Operations timeline shows create + freeze with revisions.
  await expect(page.locator('.timeline-section')).toBeVisible()
  await expect(page.locator('.timeline-section')).toContainText('freeze')
})

test('student container code-server entry', async ({ page }) => {
  await createEnvironmentFromRelease(page, '容器')
  await issueGrant(page)

  const codeServerButton = page.locator('button:has-text("打开 code-server")')
  await expect(codeServerButton).toBeEnabled()

  await expectNoA11yViolations(page, 'code-server entry should have no a11y violations')

  await page.evaluate(() => {
    window.__openedUrls = []
    window.open = (url) => {
      window.__openedUrls.push(url)
      return null
    }
  })
  await codeServerButton.click()
  const urls = await page.evaluate(() => window.__openedUrls)
  expect(urls.some((u) => u.startsWith('https://gateway.labweaver.local/connect/'))).toBe(true)
})

test('revoked grant hides runtime entries', async ({ page }) => {
  await createEnvironmentFromRelease(page, '虚拟机')
  await issueGrant(page)
  await expect(page.locator('.ssh-command__text')).toBeVisible()

  await page.locator('button:has-text("撤销授权")').click()
  await expect(page.locator('text=ACCESS_GRANT_REVOKED')).toBeVisible()
  await expect(page.locator('.ssh-command__text')).toHaveCount(0)
})

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`vm ssh access ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await createEnvironmentFromRelease(page, '虚拟机')
      await issueGrant(page)
      await expect(page.locator('.ssh-command__text')).toBeVisible()

      await expect(page).toHaveScreenshot(`vm-ssh-access-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })

    test(`container code-server entry ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await createEnvironmentFromRelease(page, '容器')
      await issueGrant(page)
      await expect(page.locator('button:has-text("打开 code-server")')).toBeVisible()

      await expect(page).toHaveScreenshot(`container-codeserver-entry-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}
