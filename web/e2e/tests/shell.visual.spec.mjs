import { test, expect } from '@playwright/test'
import { fixtureUrl, TEACHER_NAV_GROUPS } from './fixture-shell.mjs'

const breakpoints = [
  { name: 'desktop', width: 1440, height: 900 },
  { name: 'tablet', width: 840, height: 900 },
  { name: 'mobile', width: 390, height: 844 },
]

const themes = ['light', 'dark']

// The home header prints the refresh time; freeze clock and zone so goldens
// stay byte-stable across runs.
test.use({ timezoneId: 'Asia/Shanghai', locale: 'zh-CN' })
test.beforeEach(async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-09-14T08:00:00.000Z'))
})

for (const theme of themes) {
  for (const bp of breakpoints) {
    test(`home ${theme} ${bp.name} (${bp.width}x${bp.height})`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' })
      await page.setViewportSize({ width: bp.width, height: bp.height })
      await page.goto(fixtureUrl('/'))
      await page.waitForSelector('.home-view')
      await expect(page.getByRole('heading', { level: 1 })).toBeVisible()
      if (bp.name !== 'mobile') {
        // Desktop/tablet pin the drawer into the app-shell grid; assert the
        // role's task groups and home entry render so a stale golden cannot
        // mask a broken sidebar.
        const nav = page.getByRole('navigation', { name: '任务导航' })
        for (const group of TEACHER_NAV_GROUPS) {
          await expect(nav.getByRole('heading', { name: group })).toBeVisible()
        }
        await expect(nav.getByRole('link').first()).toBeVisible()
      }
      if (bp.name === 'mobile') {
        await expect(page.getByRole('button', { name: '打开导航' })).toBeVisible()
        const widths = await page.evaluate(() => ({
          viewport: document.documentElement.clientWidth,
          document: document.documentElement.scrollWidth,
          body: document.body.scrollWidth,
        }))
        expect(widths.document).toBeLessThanOrEqual(widths.viewport)
        expect(widths.body).toBeLessThanOrEqual(widths.viewport)
      }
      await expect(page).toHaveScreenshot(`home-${theme}-${bp.name}.png`, {
        fullPage: true,
        animations: 'disabled',
      })
    })
  }
}
