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
function materialDir({ suffix = '', putFail = false, runFail = false } = {}) {
  const baseName = `labweaver-material${suffix}`
  const dir = path.join(os.tmpdir(), baseName)
  if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true })
  fs.writeFileSync(path.join(dir, 'README.md'), `# Lab problem\n\ndeterministic fixture material ${suffix}\n`)
  fs.writeFileSync(path.join(dir, 'main.py'), `print("labweaver${suffix}")\n`)
  if (putFail) {
    fs.writeFileSync(path.join(dir, '__put-fail__.txt'), 'this file triggers fixture object upload failure\n')
  }
  if (runFail) {
    fs.writeFileSync(path.join(dir, '__run-fail__.txt'), 'this file triggers fixture agent run failure\n')
  }
  return dir
}

async function expectNoA11yViolations(page, message) {
  const axeResult = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze()
  expect(axeResult.violations, message).toEqual([])
}

async function uploadMaterialPackage(page, options) {
  await page.setInputFiles('input[type="file"]', materialDir(options))
  await expect(page.locator('button:has-text("上传材料包")')).toBeEnabled()
  await page.locator('button:has-text("上传材料包")').click()
  await expect(page.locator('.package-summary')).toContainText('材料包已归档')
}

async function setFixtureFlag(page, name, value) {
  await page.evaluate((kv) => {
    window.localStorage.setItem(`fixture:${kv.name}`, String(kv.value))
  }, { name, value })
}

async function removeMaterialFile(dirPath, fileName) {
  const file = path.join(dirPath, fileName)
  if (fs.existsSync(file)) fs.rmSync(file)
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
  await expect(page.locator('.run-state')).toHaveText('运行中')

  // The fixture run succeeds after a bounded number of polls.
  await expect(page.locator('.run-state')).toHaveText('已成功', { timeout: 15000 })
})

test('material upload agent run cancel', async ({ page }) => {
  await page.goto('/teacher/materials')
  await page.waitForSelector('.material-upload')
  await expect(page.locator('.policy-card')).toBeVisible()

  await uploadMaterialPackage(page)

  await page.locator('button:has-text("启动 AgentRun")').click()
  await expect(page.locator('.run-state')).toHaveText('运行中')

  await page.locator('button:has-text("取消")').click()
  await expect(page.locator('.run-state')).toHaveText('已取消', { timeout: 15000 })
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
      await expect(page.locator('.run-state')).toHaveText('已成功', { timeout: 15000 })

      await expectNoA11yViolations(page, 'run success view should have no a11y violations')

      await expect(page).toHaveScreenshot(`material-upload-run-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}

// Human fixture login: a real browser with no Playwright storageState must be
// redirected to the home page, then sign in via the deterministic fixture panel
// and land back on the protected page.
test.describe('human fixture login', () => {
  test.use({ storageState: 'e2e/fixtures/empty.json' })

  test('unauthenticated teacher reaches home and signs into materials', async ({ page }) => {
    await page.goto('/teacher/materials')
    await expect(page).toHaveURL('/')
    await expect(page.locator('.fixture-demo-roles')).toBeVisible()
    await page.locator('button:has-text("以教师身份演示")').click()
    await expect(page).toHaveURL('/teacher/materials')
    await expect(page.locator('.policy-card')).toBeVisible()
  })
})

// Loading state visual coverage: delay every fixture API response so the
// loading indicator is observable before the policy card renders.
for (const theme of themes) {
  for (const viewport of viewports) {
    test(`material upload loading ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.addInitScript(() => {
        window.localStorage.setItem('fixture:demoDelayMs', '1500')
      })
      await page.goto('/teacher/materials')
      await expect(page.locator('.material-upload')).toBeVisible()
      await expect(page.locator('.async-state-view .state-message')).toBeVisible()
      await expect(page.locator('.policy-card')).toBeVisible()

      await expect(page).toHaveScreenshot(`material-upload-loading-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}

// Upload object failure: one file in the directory is tagged `__put-fail__`, so
// the PUT is marked failed without rejecting the whole batch. After removing
// that file and retrying, the package completes normally.
for (const theme of themes) {
  for (const viewport of viewports) {
    test(`material upload object failure ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/teacher/materials')
      await page.waitForSelector('.material-upload')
      await expect(page.locator('.policy-card')).toBeVisible()

      const dir = materialDir({ suffix: '-put-fail', putFail: true })
      await page.setInputFiles('input[type="file"]', dir)
      await expect(page.locator('button:has-text("上传材料包")')).toBeEnabled()
      await page.locator('button:has-text("上传材料包")').click()

      await expect(page.locator('text=UPLOAD_OBJECT_FAILED')).toBeVisible()

      await expect(page).toHaveScreenshot(`material-upload-object-failure-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })

      // Remove the failing file from the file list and retry with the same session.
      await page.locator('tr:has-text("__put-fail__.txt") button[aria-label="移除"]').click()
      await page.locator('button:has-text("重试")').click()
      await expect(page.locator('.package-summary')).toContainText('材料包已归档')
    })
  }
}

// Conflict state: rotate the active policy after the page loads, then start a
// new upload session so the backend returns POLICY_REVISION_MISMATCH.
for (const theme of themes) {
  for (const viewport of viewports) {
    test(`material upload conflict ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/teacher/materials')
      await page.waitForSelector('.material-upload')
      await expect(page.locator('.policy-card')).toBeVisible()

      await setFixtureFlag(page, 'rotatePolicy', '1')

      const dir = materialDir({ suffix: '-conflict' })
      await page.setInputFiles('input[type="file"]', dir)
      await expect(page.locator('button:has-text("上传材料包")')).toBeEnabled()
      await page.locator('button:has-text("上传材料包")').click()

      await expect(page.locator('text=POLICY_REVISION_MISMATCH')).toBeVisible()

      await expect(page).toHaveScreenshot(`material-upload-conflict-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}

// Failed run + retry: a package containing `__run-fail__` produces a failed
// AgentRun; retrying the environment track clears the flag and succeeds.
for (const theme of themes) {
  for (const viewport of viewports) {
    test(`material upload failed run retry ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/teacher/materials')
      await page.waitForSelector('.material-upload')
      await expect(page.locator('.policy-card')).toBeVisible()

      await uploadMaterialPackage(page, { suffix: '-run-fail', runFail: true })
      await page.locator('button:has-text("启动 AgentRun")').click()
      await expect(page.locator('.run-state')).toHaveText('失败', { timeout: 15000 })

      await expect(page).toHaveScreenshot(`material-upload-run-failed-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })

      await page.locator('button:has-text("重试环境轨道")').click()
      await expect(page.locator('.run-state')).toHaveText('已成功', { timeout: 15000 })
    })
  }
}

// Poll gap + recovery: the next two poll requests fail transiently, so the UI
// shows a recoverable diagnostic; resuming polling restores the loop.
for (const theme of themes) {
  for (const viewport of viewports) {
    test(`material upload poll gap ${theme} ${viewport.name}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/teacher/materials')
      await page.waitForSelector('.material-upload')
      await expect(page.locator('.policy-card')).toBeVisible()

      await uploadMaterialPackage(page, { suffix: '-poll-gap' })
      await setFixtureFlag(page, 'agentRunPollFailures', '1')
      await page.locator('button:has-text("启动 AgentRun")').click()
      await expect(page.locator('.run-state')).toHaveText('运行中')

      await expect(page.locator('text=AGENT_RUN_POLL_TRANSIENT')).toBeVisible()

      await expect(page).toHaveScreenshot(`material-upload-poll-gap-${theme}-${viewport.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })

      await page.locator('.poll-error button:has-text("重试")').click()
      await expect(page.locator('.run-state')).toHaveText('已成功', { timeout: 15000 })
    })
  }
}

// Unauthorized role: a student trying to reach the teacher page is sent to the
// role-denied error page.
test.describe('unauthorized student', () => {
  test.use({ storageState: '.auth/student.json' })

  for (const theme of themes) {
    for (const viewport of viewports) {
      test(`material upload unauthorized ${theme} ${viewport.name}`, async ({ page }) => {
        await page.emulateMedia({ colorScheme: theme })
        await page.setViewportSize({ width: viewport.width, height: viewport.height })
        await page.goto('/teacher/materials')
        await expect(page).toHaveURL(/\/auth\/error\?reason=role_denied/)
        await expect(page.locator('text=无权访问该角色页面')).toBeVisible()

        await expect(page).toHaveScreenshot(`material-upload-unauthorized-${theme}-${viewport.name}.png`, {
          fullPage: true,
          animations: 'disabled',
        })
      })
    }
  }
})
