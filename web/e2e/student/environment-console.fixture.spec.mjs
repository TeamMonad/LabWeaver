import { test, expect } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'

const viewports = [
  { name: 'desktop', width: 1440, height: 900 },
  { name: 'mobile', width: 390, height: 844 },
]

const themes = ['light', 'dark']

async function waitForEnvironmentCard(page) {
  await page.waitForSelector('.env-card')
}

async function expectNoA11yViolations(page, message) {
  const axeResult = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze()
  expect(axeResult.violations, message).toEqual([])
}

const expectedDrawerLabels = ['教师工作台', '学生工作台', '科研工作台', '管理工作台']

// Desktop layouts pin the navigation drawer into the app-shell grid. Assert the
// role entries actually render so a stale golden baseline can never mask a
// broken sidebar again (the pixel tolerance alone is too coarse to catch it).
async function expectDesktopDrawerNav(page, viewportName) {
  if (viewportName !== 'desktop') return
  const items = page.locator('.drawer-item')
  await expect(items).toHaveCount(expectedDrawerLabels.length)
  for (const [index, label] of expectedDrawerLabels.entries()) {
    await expect(items.nth(index)).toContainText(label)
    await expect(items.nth(index)).toBeVisible()
  }
}

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`environment console empty ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/student/environments')
      await page.waitForSelector('.environment-entry')
      await expect(page.locator('text=选择版本创建环境，或输入已有环境 ID 开始管理')).toBeVisible()
      await expect(page.locator('.data-table__row')).toHaveCount(2)

      await expectNoA11yViolations(page, 'empty console should have no a11y violations')
      await expectDesktopDrawerNav(page, viewport.name)

      await expect(page).toHaveScreenshot(`environment-console-empty-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}

// The blocked student state (no course_id claim) needs a different storage
// state, which can only be overridden at describe level, not inside a test.
test.describe('environment console blocked', () => {
  test.use({ storageState: '.auth/student-blocked.json' })

  for (const theme of themes) {
    for (const viewport of viewports) {
      test(`environment console blocked ${theme} ${viewport.name}`, async ({ page }) => {
        await page.emulateMedia({ colorScheme: theme })
        await page.setViewportSize({ width: viewport.width, height: viewport.height })
        await page.goto('/student/environments')
        await page.waitForSelector('.environment-entry')
        await expect(page.locator('text=课程上下文未绑定')).toBeVisible()

        await expectNoA11yViolations(page, 'blocked console should have no a11y violations')
        await expectDesktopDrawerNav(page, viewport.name)

        await expect(page).toHaveScreenshot(`environment-console-blocked-${theme}-${viewport.name}.png`, {
          fullPage: true,
          animations: 'disabled',
        })
      })
    }
  }
})

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`environment console error ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/student/environments?environmentId=env-not-found')
      await page.waitForSelector('.environment-entry')
      await expect(page.locator('text=ENVIRONMENT_NOT_FOUND')).toBeVisible()

      await expectNoA11yViolations(page, 'error console should have no a11y violations')
      await expectDesktopDrawerNav(page, viewport.name)

      await expect(page).toHaveScreenshot(`environment-console-error-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}

test('environment console direct entry loads endpoints', async ({ page }) => {
  await page.goto('/student/environments?environmentId=env-lifecycle-failure')
  await page.waitForSelector('.environment-entry')

  // The env card should load from the direct URL.
  await waitForEnvironmentCard(page)
  await expect(page.locator('.env-state')).toHaveText('failed')

  // Endpoints must load automatically so the grant button is enabled.
  await expect(page.locator('.access-section .data-table__row')).toHaveCount(1)
  await expect(page.locator('button:has-text("签发访问授权")')).toBeEnabled()
})

test('environment console lifecycle failure shows diagnostic', async ({ page }) => {
  await page.goto('/student/environments?environmentId=env-lifecycle-failure')
  await page.waitForSelector('.environment-entry')
  await waitForEnvironmentCard(page)

  // The failed env has desiredState=running, so stop and restart are enabled.
  // Attempt stop; the fixture rejects the operation and the UI shows a diagnostic.
  await page.locator('button:has-text("停止")').click()
  await expect(page.locator('.lifecycle-result')).toContainText('ENVIRONMENT_STOP_FAILED')
  await expect(page.locator('.lifecycle-result')).toContainText('stop 失败')

  // Attempt restart; the previous diagnostic is replaced by the new one.
  await page.locator('button:has-text("重启")').click()
  await expect(page.locator('.lifecycle-result')).toContainText('ENVIRONMENT_RESTART_FAILED')
  await expect(page.locator('.lifecycle-result')).toContainText('restart 失败')
})

test('environment console create, lifecycle, grant and revoke', async ({ page }) => {
  await page.goto('/student/environments')
  await page.waitForSelector('.environment-entry')

  // Create a new environment from the first release row.
  await page.locator('.data-table__row').first().locator('button:has-text("创建环境")').click()
  await page.waitForURL(/\/student\/environments\?environmentId=/)
  await waitForEnvironmentCard(page)
  await expect(page.locator('.env-state')).toHaveText('ready')

  // Lifecycle: stop -> start -> restart.
  await page.locator('button:has-text("停止")').click()
  await expect(page.locator('.env-state')).toHaveText('stopped')

  await page.locator('button:has-text("启动")').click()
  await expect(page.locator('.env-state')).toHaveText('ready')

  await page.locator('button:has-text("重启")').click()
  await expect(page.locator('.env-state')).toHaveText('ready')

  // Issue an access grant.
  await page.locator('button:has-text("签发访问授权")').click()
  await expect(page.locator('.grant-card')).toBeVisible()
  await expect(page.locator('text=active')).toBeVisible()

  // Revoke the grant and observe the revoked diagnostic.
  await page.locator('button:has-text("撤销授权")').click()
  await expect(page.locator('text=访问授权已撤销')).toBeVisible()
})

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`environment console success ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/student/environments')
      await page.waitForSelector('.environment-entry')

      await page.locator('.data-table__row').first().locator('button:has-text("创建环境")').click()
      await page.waitForURL(/\/student\/environments\?environmentId=/)
      await waitForEnvironmentCard(page)
      await expect(page.locator('.env-state')).toHaveText('ready')

      await expectNoA11yViolations(page, 'success console should have no a11y violations')
      await expectDesktopDrawerNav(page, viewport.name)

      await expect(page).toHaveScreenshot(`environment-console-success-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`environment console grant ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/student/environments')
      await page.waitForSelector('.environment-entry')

      await page.locator('.data-table__row').first().locator('button:has-text("创建环境")').click()
      await page.waitForURL(/\/student\/environments\?environmentId=/)
      await waitForEnvironmentCard(page)
      await expect(page.locator('.env-state')).toHaveText('ready')

      await page.locator('button:has-text("签发访问授权")').click()
      await expect(page.locator('.grant-card')).toBeVisible()

      await expectNoA11yViolations(page, 'grant view should have no a11y violations')
      await expectDesktopDrawerNav(page, viewport.name)

      await expect(page).toHaveScreenshot(`environment-console-grant-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`environment console revoked ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/student/environments')
      await page.waitForSelector('.environment-entry')

      await page.locator('.data-table__row').first().locator('button:has-text("创建环境")').click()
      await page.waitForURL(/\/student\/environments\?environmentId=/)
      await waitForEnvironmentCard(page)
      await expect(page.locator('.env-state')).toHaveText('ready')

      await page.locator('button:has-text("签发访问授权")').click()
      await expect(page.locator('.grant-card')).toBeVisible()

      await page.locator('button:has-text("撤销授权")').click()
      await expect(page.locator('text=访问授权已撤销')).toBeVisible()

      await expectNoA11yViolations(page, 'revoked view should have no a11y violations')
      await expectDesktopDrawerNav(page, viewport.name)

      await expect(page).toHaveScreenshot(`environment-console-revoked-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}
