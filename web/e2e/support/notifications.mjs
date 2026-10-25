import { expect } from '@playwright/test'
import { expectJson } from './live.mjs'

// Control writes the notification from the resource event stream, so it lands
// a little after the decision; the top bar badge itself polls once a minute.
const NOTIFICATION_ARRIVAL_TIMEOUT_MS = 180_000
const NOTIFICATION_SETTLE_TIMEOUT_MS = 30_000

const EMAIL_STATE_LABEL = Object.freeze({
  disabled: '仅站内通知',
  pending: '邮件待发送',
  sent: '邮件已发送',
  failed: '邮件发送失败',
  exhausted: '邮件未能发送',
  no_verified_email: '未发送邮件',
  delivery_unknown: '邮件投递结果未知',
})

function isNotificationListResponse(response) {
  const url = new URL(response.url())
  return response.request().method() === 'GET' && url.pathname === '/api/v1/notifications'
}

/**
 * Open the reader's notification center from the top bar, wait for the
 * resource decision notification about `requestId`, open it, and check that it
 * lands on the located request row and is then counted as read.
 */
export async function openResourceDecisionNotificationByUi(page, { projectId, requestId }) {
  const expectedObjectUrl = `/researcher/resources?projectId=${projectId}&requestId=${requestId}`
  const entry = page.getByTestId('notification-entry')
  await expect(entry).toBeVisible()

  let notification = null
  await expect.poll(async () => {
    const listResponse = page.waitForResponse(isNotificationListResponse)
    if (new URL(page.url()).pathname === '/notifications') {
      await page.getByRole('button', { name: '刷新通知', exact: true }).click()
    } else {
      await entry.click()
    }
    const pageData = await expectJson(await listResponse, 'LW_ACCEPTANCE_NOTIFICATION_LIST_FAILED')
    notification = pageData.items.find((item) => item.kind === 'resource_decision' && item.objectUrl === expectedObjectUrl) ?? null
    return notification !== null
  }, { timeout: NOTIFICATION_ARRIVAL_TIMEOUT_MS, intervals: [2_000, 5_000, 10_000] }).toBe(true)
  await expect(page).toHaveURL(/\/notifications$/)
  expect(notification.projectId).toBe(projectId)
  expect(notification.readAt ?? null).toBeNull()

  // The badge and its accessible name are server counts, never local guesses.
  const countResponse = await expectJson(
    await page.request.get('/api/v1/notifications/unread-count'),
    'LW_ACCEPTANCE_NOTIFICATION_UNREAD_COUNT_FAILED',
  )
  expect(countResponse.unreadCount).toBeGreaterThan(0)

  const row = page.locator(`[data-testid="notification-item"][data-notification-id="${notification.id}"]`)
  await expect(row).toBeVisible()
  await expect(row).toContainText('资源申请结果')
  await expect(row).toContainText('未读')
  await expect(row).toContainText(notification.summary)
  await expect(row.getByTestId('notification-email-status')).toContainText(EMAIL_STATE_LABEL[notification.emailState])
  if (notification.emailState !== 'sent') {
    await expect(row.getByTestId('notification-email-status')).not.toContainText('邮件已发送')
  }

  const readResponsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/notifications/${notification.id}/read`
  })
  await row.getByRole('button').first().click()
  const read = await expectJson(await readResponsePromise, 'LW_ACCEPTANCE_NOTIFICATION_MARK_READ_FAILED')
  expect(read.id).toBe(notification.id)
  expect(read.readAt).toEqual(expect.any(String))

  await expect(page).toHaveURL((url) => url.pathname === '/researcher/resources'
    && url.searchParams.get('projectId') === projectId
    && url.searchParams.get('requestId') === requestId)
  const located = page.getByTestId('resource-request-located')
  await expect(located).toBeVisible({ timeout: NOTIFICATION_SETTLE_TIMEOUT_MS })
  await expect(located).toHaveAttribute('id', `resource-request-${requestId}`)
  await expect(page.getByTestId('resource-request-not-found')).toHaveCount(0)

  // Opening it again from the list is a plain link: already read, no second write.
  await entry.click()
  await expect(page).toHaveURL(/\/notifications$/)
  const readRow = page.locator(`[data-testid="notification-item"][data-notification-id="${notification.id}"]`)
  await expect(readRow).toContainText('已读', { timeout: NOTIFICATION_SETTLE_TIMEOUT_MS })
  let secondWrite = false
  const watchWrites = (request) => {
    if (request.method() === 'POST' && new URL(request.url()).pathname.startsWith('/api/v1/notifications/')) secondWrite = true
  }
  page.on('request', watchWrites)
  try {
    await readRow.getByRole('button').first().click()
    await expect(page.getByTestId('resource-request-located')).toBeVisible({ timeout: NOTIFICATION_SETTLE_TIMEOUT_MS })
  } finally {
    page.off('request', watchWrites)
  }
  expect(secondWrite, 'LW_ACCEPTANCE_NOTIFICATION_READ_REPEATED').toBe(false)
  return { notificationId: notification.id, emailState: notification.emailState }
}
