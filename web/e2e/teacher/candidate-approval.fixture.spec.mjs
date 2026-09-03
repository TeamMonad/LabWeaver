import { test, expect } from '@playwright/test'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

const viewports = [
  { name: 'desktop', width: 1440, height: 900 },
  { name: 'mobile', width: 390, height: 844 },
]

const themes = ['light', 'dark']

function materialDir() {
  const dir = path.join(os.tmpdir(), 'lw-approval-material')
  if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true })
  fs.writeFileSync(path.join(dir, 'README.md'), '# Lab problem\n\napproval e2e\n')
  fs.writeFileSync(path.join(dir, 'main.py'), 'print("approval")\n')
  return dir
}

async function uploadAndStartRun(page) {
  await page.goto('/teacher/materials')
  await page.waitForSelector('.material-upload')
  await expect(page.locator('.policy-card')).toBeVisible()
  await page.setInputFiles('input[type="file"]', materialDir())
  await page.locator('button:has-text("上传材料包")').click()
  await expect(page.locator('.package-summary')).toContainText('材料包已归档')
  await page.locator('button:has-text("启动 AgentRun")').click()
  await expect(page.locator('.run-state')).toHaveText('已成功', { timeout: 15000 })
}

async function openApprovalPage(page) {
  await page.locator('a:has-text("进入候选审批")').click()
  await expect(page).toHaveURL(/\/teacher\/approvals\?runId=/)
  await expect(page.locator('.candidate-approval')).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Environment 候选' })).toBeVisible()
}

async function setImageGate(page, scenario) {
  await page.evaluate((s) => {
    window.localStorage.setItem('fixture:imageGate', s)
  }, scenario)
}

test('candidate approval flow publishes and withdraws one release under a duplicate confirm event', async ({ page }) => {
  await uploadAndStartRun(page)
  await openApprovalPage(page)

  await page.fill('textarea[aria-label="Environment 审批理由"]', 'Environment candidate approved')
  await page.locator('button:has-text("批准")').first().click()
  await expect(page.locator('.approval-status--approved')).toBeVisible()

  await page.fill('textarea[aria-label="Evaluation 审批理由"]', 'Evaluation candidate acknowledged')
  await page.locator('button:has-text("批准")').last().click()
  await expect(page.locator('text=最新审批：approved — Evaluation candidate acknowledged')).toBeVisible()

  await page.locator('button:has-text("发布 EvaluationRelease")').click()
  await page.getByRole('alertdialog').getByRole('button', { name: '发布', exact: true }).evaluate((button) => {
    button.click()
    button.click()
  })
  await expect(page.locator('text=EvaluationRelease 已发布')).toBeVisible()
  // The fixture starts with one release. Two synchronous confirmation events
  // must replay one idempotency fence instead of creating two more releases.
  await expect(page.locator('.release-list > li')).toHaveCount(2)
  await expect(page.getByText('受控 Runtime 身份（只读）')).toBeVisible()
  await expect(page.getByText('kubernetes/evaluation-primary-v1')).toBeVisible()

  await page.locator('.release-list .outlined-button').first().click()
  await page.getByRole('alertdialog').getByRole('button', { name: '撤回', exact: true }).click()
  await expect(page.getByText('该 Release 已撤回；历史终态结果仍保留。')).toBeVisible()

  await page.locator('button:has-text("发布 EnvironmentTemplateRelease")').click()
  await page.getByRole('button', { name: '发布', exact: true }).click()
  await expect(page.locator('text=已接受发布请求')).toBeVisible()
})

test('critical vulnerability blocks publish', async ({ page }) => {
  await uploadAndStartRun(page)
  await setImageGate(page, 'critical')
  await openApprovalPage(page)

  await page.fill('textarea[aria-label="Environment 审批理由"]', 'Environment candidate approved')
  await page.locator('button:has-text("批准")').first().click()
  await expect(page.locator('.approval-status--approved')).toBeVisible()

  await expect(page.locator('text=Critical 1')).toBeVisible()
  await expect(page.locator('button:has-text("发布 EnvironmentTemplateRelease")')).toBeDisabled()
})

test('high vulnerability shows warning but allows publish', async ({ page }) => {
  await uploadAndStartRun(page)
  await setImageGate(page, 'high')
  await openApprovalPage(page)

  await page.fill('textarea[aria-label="Environment 审批理由"]', 'Environment candidate approved')
  await page.locator('button:has-text("批准")').first().click()
  await expect(page.locator('.approval-status--approved')).toBeVisible()

  await expect(page.locator('text=High 1')).toBeVisible()
  await expect(page.locator('button:has-text("发布 EnvironmentTemplateRelease")')).toBeEnabled()
})

test('release evidence excludes the retired signing trust plane', async ({ page }) => {
  await uploadAndStartRun(page)
  await openApprovalPage(page)

  await page.fill('textarea[aria-label="Environment 审批理由"]', 'Environment candidate approved')
  await page.locator('button:has-text("批准")').first().click()
  await expect(page.locator('.approval-status--approved')).toBeVisible()

  await expect(page.getByText('Digest', { exact: true })).toBeVisible()
  await expect(page.getByText('Trivy', { exact: true })).toBeVisible()
  await expect(page.getByText('Immutable Tag', { exact: true })).toHaveCount(0)
  await expect(page.getByText('SBOM', { exact: true })).toHaveCount(0)
  await expect(page.locator('button:has-text("发布 EnvironmentTemplateRelease")')).toBeEnabled()
})

test('wrong digest blocks publish', async ({ page }) => {
  await uploadAndStartRun(page)
  await setImageGate(page, 'wrong-digest')
  await openApprovalPage(page)

  await page.fill('textarea[aria-label="Environment 审批理由"]', 'Environment candidate approved')
  await page.locator('button:has-text("批准")').first().click()
  await expect(page.locator('.approval-status--approved')).toBeVisible()

  await expect(page.locator('button:has-text("发布 EnvironmentTemplateRelease")')).toBeDisabled()
})

test('duplicate decision conflicts', async ({ page }) => {
  await uploadAndStartRun(page)
  await openApprovalPage(page)

  await page.fill('textarea[aria-label="Environment 审批理由"]', 'Environment candidate approved')
  await page.locator('button:has-text("批准")').first().click()
  await expect(page.locator('.approval-status--approved')).toBeVisible()

  await page.fill('textarea[aria-label="Environment 审批理由"]', 'Environment candidate approved')
  await page.locator('button:has-text("批准")').first().click()
  await expect(page.locator('text=FIXTURE_CONFLICT')).toBeVisible()
})

test.describe('unauthorized student', () => {
  test.use({ storageState: '.auth/student.json' })

  test('student cannot access approval page', async ({ page }) => {
    await page.goto('/teacher/approvals?runId=run-1')
    await expect(page).toHaveURL(/\/auth\/error\?reason=role_denied/)
  })
})

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`approval policy ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await uploadAndStartRun(page)
      await openApprovalPage(page)

      await expect(page).toHaveScreenshot(`approval-policy-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })

    test(`approval evidence ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await uploadAndStartRun(page)
      await openApprovalPage(page)

      await page.fill('textarea[aria-label="Environment 审批理由"]', 'Environment candidate approved')
      await page.locator('button:has-text("批准")').first().click()
      await expect(page.locator('.approval-status--approved')).toBeVisible()

      await expect(page).toHaveScreenshot(`approval-evidence-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}
