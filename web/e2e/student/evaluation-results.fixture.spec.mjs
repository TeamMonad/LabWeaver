import { test, expect } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'

const surfaces = [
  { theme: 'light', viewport: { width: 1440, height: 900 } },
  { theme: 'dark', viewport: { width: 390, height: 844 } },
]

for (const { theme, viewport } of surfaces) {
  test(`terminal results are accessible in ${theme} at ${viewport.width}px`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: theme })
    await page.setViewportSize(viewport)
    await page.goto('/student/results')
    await expect(page.getByRole('heading', { name: '评测结果' })).toBeVisible()
    await expect(page.locator('.result-card')).toHaveCount(3)
    await expect(page.getByText('92 / 100')).toBeVisible()
    await expect(page.getByText('评测失败，未产生可发布的总分。')).toBeVisible()
    await expect(page.getByText('评测已取消，未产生可发布的总分。')).toBeVisible()
    const axe = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze()
    expect(axe.violations).toEqual([])
  })
}

test('opens a successful result detail without polling or sensitive internals', async ({ page }) => {
  await page.goto('/student/results')
  const succeeded = page.locator('.result-card').filter({ hasText: '成功' })
  await succeeded.locator('a').click()
  await expect(page.getByRole('heading', { name: '评测详情' })).toBeVisible()
  await expect(page.getByText('最终总分')).toBeVisible()
  await expect(page.getByRole('article').getByText('92 / 100')).toBeVisible()
  await expect(page.getByText('公开步骤')).toBeVisible()
  await expect(page.getByText(/evidence|runtime identity|step id|submission content/i)).toHaveCount(0)
})

test('renders empty and error states explicitly', async ({ page }) => {
  await page.goto('/student/results')
  await page.evaluate(() => localStorage.setItem('fixture:evaluationResults', 'empty'))
  await page.reload()
  await expect(page.getByText('当前课程暂无终态评测结果')).toBeVisible()

  await page.evaluate(() => localStorage.setItem('fixture:evaluationResults', 'error'))
  await page.reload()
  await expect(page.getByText('LW_EVALUATION_UNAVAILABLE')).toBeVisible()
})

test('course isolation fails closed for an out-of-scope course claim', async ({ page }) => {
  await page.goto('/student/results')
  await page.evaluate(() => {
    const key = Object.keys(localStorage).find((name) => name.startsWith('oidc.user:'))
    if (!key) throw new Error('fixture OIDC state is missing')
    const user = JSON.parse(localStorage.getItem(key))
    user.profile.course_id = 'course-102'
    localStorage.setItem(key, JSON.stringify(user))
  })
  await page.reload()
  await expect(page.getByText('FORBIDDEN')).toBeVisible()
  await expect(page.locator('.result-card')).toHaveCount(0)
})

test.describe('teacher role denial', () => {
  test.use({ storageState: '.auth/teacher.json' })
  test('teacher cannot access student results', async ({ page }) => {
    await page.goto('/student/results')
    await expect(page).toHaveURL(/\/auth\/error\?reason=role_denied/)
  })
})
