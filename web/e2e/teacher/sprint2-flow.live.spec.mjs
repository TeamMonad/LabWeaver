import { expect, test } from '@playwright/test'

function requiredEnvironment(name) {
  const value = process.env[name]?.trim()
  if (!value) throw new Error(`PW_RUNTIME_INPUT_MISSING:${name}`)
  return value
}

test('teacher sees the ECNU policy and both approved AgentRun candidates', async ({ page }) => {
  await page.goto('/teacher/materials', { waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: '材料上传与 AgentRun' })).toBeVisible()
  await expect(page.getByText('课程 LLM 出站策略')).toBeVisible()
  await expect(page.getByText('qwen3.6:27b', { exact: true })).toBeVisible()

  const runId = requiredEnvironment('LABWEAVER_E2E_AGENT_RUN_ID')
  await page.goto(`/teacher/approvals?runId=${encodeURIComponent(runId)}`, {
    waitUntil: 'domcontentloaded',
  })
  await expect(page.getByRole('heading', { name: '候选审批与发布' })).toBeVisible()
  await expect(page.getByText(runId, { exact: true })).toBeVisible()
  await expect(page.locator('.candidate-card')).toHaveCount(2)
  await expect(page.locator('.approval-status')).toHaveCount(2)
  await expect(page.locator('.approval-status')).toHaveText([
    /最新审批：approved/,
    /最新审批：approved/,
  ])
  await expect(page.getByText('Sprint 2 不执行 EvaluationRun')).toBeVisible()
})
