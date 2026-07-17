import { test, expect } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

const viewports = [
  { name: 'desktop', width: 1440, height: 900 },
  { name: 'mobile', width: 390, height: 844 },
]

const themes = ['light', 'dark']

// webkitdirectory inputs require a real directory on disk. The directory name
// becomes part of webkitRelativePath, so it must stay deterministic across
// runs and platforms; contents are fixed to keep package hashes stable.
function materialDir() {
  const dir = path.join(os.tmpdir(), 'labweaver-material')
  if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true })
  fs.writeFileSync(path.join(dir, 'README.md'), '# Lab problem\n\ndeterministic fixture material\n')
  fs.writeFileSync(path.join(dir, 'main.py'), 'print("labweaver")\n')
  return dir
}

async function expectNoA11yViolations(page, message) {
  const axeResult = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze()
  expect(axeResult.violations, message).toEqual([])
}

async function uploadMaterialPackage(page) {
  await page.setInputFiles('input[type="file"]', materialDir())
  await expect(page.locator('button:has-text("上传材料包")')).toBeEnabled()
  await page.locator('button:has-text("上传材料包")').click()
  await expect(page.locator('.package-summary')).toContainText('材料包已归档')
}

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`material upload policy ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/teacher/materials')
      await page.waitForSelector('.material-upload')

      // LLM egress policy card renders from the fixture policy.
      await expect(page.locator('.policy-card')).toBeVisible()
      await expect(page.locator('.policy-card')).toContainText('claude-sonnet-4-5')
      await expect(page.locator('.policy-card .tag--deny').first()).toBeVisible()
      await expect(page.locator('.drop-zone')).toBeVisible()

      await expectNoA11yViolations(page, 'policy view should have no a11y violations')

      await expect(page).toHaveScreenshot(`material-upload-policy-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}

// The blocked teacher state (no course_id claim) needs a different storage
// state, which can only be overridden at describe level, not inside a test.
test.describe('material upload blocked', () => {
  test.use({ storageState: '.auth/teacher-blocked.json' })

  for (const theme of themes) {
    for (const viewport of viewports) {
      test(`material upload blocked ${theme} ${viewport.name}`, async ({ page }) => {
        await page.emulateMedia({ colorScheme: theme })
        await page.setViewportSize({ width: viewport.width, height: viewport.height })
        await page.goto('/teacher/materials')
        await page.waitForSelector('.material-upload')
        await expect(page.locator('text=课程上下文未绑定')).toBeVisible()

        await expectNoA11yViolations(page, 'blocked view should have no a11y violations')

        await expect(page).toHaveScreenshot(`material-upload-blocked-${theme}-${viewport.name}.png`, {
          fullPage: true,
          animations: 'disabled',
        })
      })
    }
  }
})

test('material upload package, agent run succeeds', async ({ page }) => {
  await page.goto('/teacher/materials')
  await page.waitForSelector('.material-upload')
  await expect(page.locator('.policy-card')).toBeVisible()

  await uploadMaterialPackage(page)

  // Start an agent run against the archived package.
  await page.locator('button:has-text("启动 AgentRun")').click()
  await expect(page.locator('.run-card')).toBeVisible()
  await expect(page.locator('.run-state')).toHaveText('running')

  // The fixture run succeeds after a bounded number of polls.
  await expect(page.locator('.run-state')).toHaveText('succeeded', { timeout: 15000 })
})

test('material upload agent run cancel', async ({ page }) => {
  await page.goto('/teacher/materials')
  await page.waitForSelector('.material-upload')
  await expect(page.locator('.policy-card')).toBeVisible()

  await uploadMaterialPackage(page)

  await page.locator('button:has-text("启动 AgentRun")').click()
  await expect(page.locator('.run-state')).toHaveText('running')

  await page.locator('button:has-text("取消")').click()
  await expect(page.locator('.run-state')).toHaveText('cancelled', { timeout: 15000 })
})

for (const theme of themes) {
  for (const viewport of viewports) {
    test(`material upload run success ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/teacher/materials')
      await page.waitForSelector('.material-upload')
      await expect(page.locator('.policy-card')).toBeVisible()

      await uploadMaterialPackage(page)
      await page.locator('button:has-text("启动 AgentRun")').click()
      await expect(page.locator('.run-state')).toHaveText('succeeded', { timeout: 15000 })

      await expectNoA11yViolations(page, 'run success view should have no a11y violations')

      await expect(page).toHaveScreenshot(`material-upload-run-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}
