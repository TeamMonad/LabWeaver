import { expect } from '@playwright/test'
import { expectJson, selectProjectByUi } from './live.mjs'

const REVIEW_TIMEOUT_MS = 120_000

function approvalsPath(projectId, query = {}) {
  return `/teacher/approvals?${new URLSearchParams({ projectId, ...query }).toString()}`
}

/**
 * Opens one authoring run on the review page the way a teacher does: from the
 * project's review queue, without typing or pasting an internal ID.
 */
export async function openAuthoringReviewByUi(page, projectId, runId) {
  await page.goto(approvalsPath(projectId), { waitUntil: 'domcontentloaded' })
  await selectProjectByUi(page, projectId)
  await expect(page.getByRole('heading', { name: '审核与发布', exact: true })).toBeVisible()
  await expect(page.locator('input[aria-label="AgentRun ID"]')).toHaveCount(0)
  const queue = page.getByTestId('review-queue')
  const entry = queue.locator(`a[data-run-id="${runId}"]`)
  await expect(entry).toHaveCount(1, { timeout: REVIEW_TIMEOUT_MS })
  await entry.click()
  await expect(page).toHaveURL(new RegExp(`[?&]runId=${encodeURIComponent(runId)}(?:&|$)`))
  await expect(entry).toHaveAttribute('aria-current', 'page')
}

/** Reopens a review from a shared link (refresh or deep link from the home page). */
export async function reopenAuthoringReviewByLink(page, projectId, runId, approvalId) {
  await page.goto(approvalsPath(projectId, { runId, ...(approvalId ? { approvalId } : {}) }), { waitUntil: 'domcontentloaded' })
  await selectProjectByUi(page, projectId)
  await expect(page.getByRole('heading', { name: '审核与发布', exact: true })).toBeVisible()
  await expect(page.getByTestId('review-queue').locator(`a[data-run-id="${runId}"]`)).toHaveAttribute('aria-current', 'page', { timeout: REVIEW_TIMEOUT_MS })
}

/** Shows the identities the approval will bind in the “技术详情” tab. */
export async function expectReviewTechnicalIdentitiesByUi(page, identities) {
  await page.getByRole('tab', { name: '技术详情', exact: true }).click()
  const panel = page.getByRole('tabpanel', { name: '技术详情', exact: true })
  for (const value of identities.filter(Boolean)) {
    await expect(panel.getByText(value, { exact: true }).first()).toBeVisible({ timeout: REVIEW_TIMEOUT_MS })
  }
  await page.getByRole('tab', { name: '概览', exact: true }).click()
}

/** Confirms, writes the reason and submits the single “批准并发布” command. */
export async function approveAndPublishByUi(page, projectId, reason, { label = 'AUTHORING_APPROVAL_FAILED', enableTimeout = 240_000 } = {}) {
  const card = page.getByTestId('approve-card')
  await expect(card).toBeVisible({ timeout: REVIEW_TIMEOUT_MS })
  const button = card.getByTestId('approve-publish')
  await expect(button).toBeDisabled()
  await card.getByRole('checkbox').check()
  await card.locator('textarea').fill(reason)
  await expect(button).toBeEnabled({ timeout: enableTimeout })

  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/projects/${projectId}/authoring-approvals`
  })
  await button.click()
  const response = await responsePromise
  const approval = await expectJson(response, label)
  expect(response.request().postDataJSON()).toMatchObject({ projectId, reason })
  await expect(page).toHaveURL(new RegExp(`[?&]approvalId=${encodeURIComponent(approval.id)}(?:&|$)`))
  return approval
}

/** Waits until the page shows the publication state the server reports. */
export async function expectPublicationStatusByUi(page, status, timeout = REVIEW_TIMEOUT_MS) {
  await expect(page.getByTestId('publication-status')).toHaveAttribute('data-status', status, { timeout })
}

/**
 * After publication the run is listed under “已发布” and the page no longer
 * offers a second approval for the same candidate.
 */
export async function expectPublishedReviewByUi(page, projectId, runId) {
  await reopenAuthoringReviewByLink(page, projectId, runId)
  const published = page.getByTestId('review-queue').locator('section[data-group="published"]')
  await expect(published.locator(`a[data-run-id="${runId}"]`)).toHaveCount(1, { timeout: REVIEW_TIMEOUT_MS })
  await expect(page.getByTestId('review-published')).toBeVisible({ timeout: REVIEW_TIMEOUT_MS })
  await expect(page.getByTestId('approve-publish')).toHaveCount(0)
}
