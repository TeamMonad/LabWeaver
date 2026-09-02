import { test, expect } from '@playwright/test'

test('fixture banner is not clipped at 390px', async ({ page }) => {
  test.skip(
    (process.env.LABWEAVER_DATA_MODE || process.env.VITE_DATA_MODE || 'live') !== 'fixture',
    'The fixture banner is intentionally absent from the connected live portal',
  )
  await page.emulateMedia({ colorScheme: 'light' })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/')
  const banner = page.locator('[data-testid="fixture-banner"]')
  await banner.waitFor({ state: 'visible' })

  const scrollWidth = await banner.evaluate((el) => el.scrollWidth)
  const clientWidth = await banner.evaluate((el) => el.clientWidth)
  expect(scrollWidth, 'banner content should fit within viewport').toBeLessThanOrEqual(clientWidth)

  const text = await banner.textContent()
  expect(text).toContain('FIXTURE MODE')
  expect(text).toContain('确定性本地 fixture')
})
