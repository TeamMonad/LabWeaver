import { test, expect } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'

const viewports = [
  { name: 'desktop', width: 1440, height: 900 },
  { name: 'mobile', width: 390, height: 844 },
]

const themes = ['light', 'dark']

async function openApprovalPage(page) {
  await page.goto('/admin/resource-approval')
  await expect(page.locator('.resource-approval')).toBeVisible()
  await expect(page.getByRole('heading', { name: '资源申请' })).toBeVisible()
  await expect(page.locator('.request-table .data-table__row').first()).toBeVisible()
  await expect(page.locator('.lease-table .data-table__row').first()).toBeVisible()
}

async function selectRequest(page, requestKey) {
  await page.locator('.request-table .data-table__row', { hasText: requestKey }).click()
  await expect(page.locator('.request-detail')).toBeVisible()
  await expect(page.locator('.request-detail')).toContainText(requestKey)
}

async function confirmDialog(page) {
  await page.getByRole('alertdialog').getByRole('button', { name: '确认' }).click()
}

test('admin approves a reviewing request and a lease appears', async ({ page }) => {
  await openApprovalPage(page)
  await selectRequest(page, 'cpu-lab-request')

  await page.fill('textarea[aria-label="资源申请操作理由"]', 'Approve for the sprint demo')
  await page.getByRole('button', { name: '批准', exact: true }).click()
  await confirmDialog(page)

  await expect(page.locator('.diagnostic-banner--info')).toContainText('RESOURCE_REQUEST_APPROVED')
  await expect(page.locator('.request-detail')).toContainText('allocating')
  await expect(page.locator('.lease-table .data-table__row')).toHaveCount(3)
})

test('admin renews and revokes an active lease', async ({ page }) => {
  await openApprovalPage(page)

  await page.locator('.lease-table .data-table__row', { hasText: 'active' }).first().click()
  await expect(page.locator('.lease-detail')).toBeVisible()

  await page.fill('input[aria-label="续期时长（秒）"]', '3600')
  await page.fill('textarea[aria-label="Lease 操作理由"]', 'Extend the active window')
  await page.getByRole('button', { name: '续期', exact: true }).click()
  await confirmDialog(page)
  await expect(page.locator('.diagnostic-banner--info')).toContainText('RESOURCE_LEASE_RENEWED')
  await expect(page.locator('.lease-detail')).toContainText('rev-3')

  await page.fill('textarea[aria-label="Lease 操作理由"]', 'Revoke after policy violation')
  await page.getByRole('button', { name: '撤销 Lease', exact: true }).click()
  await confirmDialog(page)
  await expect(page.locator('.diagnostic-banner--info')).toContainText('RESOURCE_LEASE_REVOKED')
  await expect(page.locator('.lease-detail')).toContainText('revoked')
})

test('revision conflict surfaces a stable diagnostic', async ({ page }) => {
  await openApprovalPage(page)
  await selectRequest(page, 'cpu-lab-request')

  await page.fill('textarea[aria-label="资源申请操作理由"]', 'Approve with stale revision')
  await page.evaluate(() => {
    window.localStorage.setItem('fixture:resourceRevisionConflict', '1')
  })
  await page.getByRole('button', { name: '批准', exact: true }).click()
  await confirmDialog(page)

  await expect(page.locator('.diagnostic-banner--error')).toContainText('PRECONDITION_FAILED')
})

test('reject requires a reason and rejects a reviewing request', async ({ page }) => {
  await openApprovalPage(page)
  await selectRequest(page, 'gpu-training-request')

  const rejectButton = page.getByRole('button', { name: '拒绝', exact: true })
  await expect(rejectButton).toBeDisabled()

  await page.fill('textarea[aria-label="资源申请操作理由"]', 'Quota exceeded for this course')
  await expect(rejectButton).toBeEnabled()
  await rejectButton.click()
  await confirmDialog(page)

  await expect(page.locator('.diagnostic-banner--info')).toContainText('RESOURCE_REQUEST_REJECTED')
  await expect(page.locator('.request-detail')).toContainText('rejected')
})

test('course filter narrows the request list', async ({ page }) => {
  await openApprovalPage(page)
  await expect(page.locator('.request-table .data-table__row')).toHaveCount(4)

  await page.selectOption('select[aria-label="按课程过滤资源申请"]', 'course-102')
  await expect(page.locator('.request-table .data-table__row')).toHaveCount(2)
  await expect(page.locator('.request-table')).not.toContainText('cpu-lab-request')
})

test('loaded approval page has no accessibility violations', async ({ page }) => {
  await openApprovalPage(page)
  await selectRequest(page, 'cpu-lab-request')

  const axeResult = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze()
  expect(axeResult.violations, 'resource approval page should have no a11y violations').toEqual([])
})

test.describe('unauthorized student', () => {
  test.use({ storageState: '.auth/student.json' })

  test('student is refused by the route guard', async ({ page }) => {
    await page.goto('/admin/resource-approval')
    await expect(page).toHaveURL(/\/auth\/error\?reason=role_denied/)
  })
})

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`request list ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await openApprovalPage(page)

      await expect(page.locator('.request-section')).toHaveScreenshot(`resource-approval-list-${theme}-${viewport.name}.png`, {
        animations: 'disabled',
      })
    })

    test(`request detail ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await openApprovalPage(page)
      await selectRequest(page, 'cpu-lab-request')

      await expect(page.locator('.request-detail')).toHaveScreenshot(`resource-approval-detail-${theme}-${viewport.name}.png`, {
        animations: 'disabled',
      })
    })
  }
}
