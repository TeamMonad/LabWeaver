import { expect, test } from '@playwright/test'

function requiredEnvironment(name) {
  const value = process.env[name]?.trim()
  if (!value) throw new Error(`PW_RUNTIME_INPUT_MISSING:${name}`)
  return value
}

async function loadEnvironment(page, id, runtimeLabel) {
  await page.getByLabel('环境 ID').fill(id)
  await page.getByRole('button', { name: '加载', exact: true }).click()
  await expect(page.locator('.env-id')).toHaveText(id)
  await expect(page.locator('.env-runtime')).toHaveText(runtimeLabel)
  await expect(page.getByRole('table', { name: '环境入口' })).toBeVisible()
  await expect(page.getByText('Object Version', { exact: true })).toBeVisible()
  await expect(page.getByText('SHA-256', { exact: true })).toBeVisible()
}

test('student reads frozen Container and KubeVirt runtime evidence', async ({ page }) => {
  await page.goto('/student/environments')
  await expect(page.getByRole('heading', { name: '环境控制台', exact: true }).first()).toBeVisible()

  await loadEnvironment(
    page,
    requiredEnvironment('LABWEAVER_E2E_CONTAINER_ENVIRONMENT_ID'),
    '容器',
  )
  await loadEnvironment(
    page,
    requiredEnvironment('LABWEAVER_E2E_VM_ENVIRONMENT_ID'),
    '虚拟机',
  )
})
