import { expect, test } from '@playwright/test'
import { createProjectByUi, uuidv7 } from '../support/live.mjs'

/**
 * Project context journey (#201): deep links, refresh and switching keep the
 * address, selector, title and requests on one project, and a link to a
 * project the account cannot open shows a recovery page instead of the
 * previous project's data. Every step uses the visible console.
 */

const PAGE_TIMEOUT_MS = 60_000

function projectRequestRecorder(page) {
  const paths = []
  page.on('request', (request) => {
    const pathname = new URL(request.url()).pathname
    if (pathname.startsWith('/api/v1/projects/')) paths.push(pathname)
  })
  return {
    since(mark) {
      return paths.slice(mark)
    },
    mark() {
      return paths.length
    },
  }
}

async function expectProjectContext(page, project) {
  await expect.poll(() => new URL(page.url()).searchParams.get('projectId'), { timeout: PAGE_TIMEOUT_MS }).toBe(project.id)
  const trigger = page.getByRole('button', { name: '选择项目', exact: true })
  await expect(trigger.locator('.trigger-primary')).toHaveText(project.name, { timeout: PAGE_TIMEOUT_MS })
  await expect(page).toHaveTitle(new RegExp(`· ${project.name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')} · LabWeaver$`), { timeout: PAGE_TIMEOUT_MS })
}

function expectOnlyProjectRequests(paths, project, other) {
  expect(paths.some((path) => path.startsWith(`/api/v1/projects/${project.id}`))).toBe(true)
  expect(paths.filter((path) => path.startsWith(`/api/v1/projects/${other.id}`))).toEqual([])
}

test('project context follows deep links, refresh and keyboard switching, and recovers from an unavailable link', async ({ page }) => {
  const suffix = uuidv7().slice(-8)
  const first = await createProjectByUi(page, `上下文 A ${suffix}`)
  const second = await createProjectByUi(page, `上下文 B ${suffix}`)
  // The selection is now the second project; a deep link to the first must win over it.
  const requests = projectRequestRecorder(page)

  let mark = requests.mark()
  await page.goto(`/researcher/workspaces?projectId=${encodeURIComponent(first.id)}&releaseId=${encodeURIComponent(uuidv7())}`, { waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: '项目与工作空间', exact: true })).toBeVisible({ timeout: PAGE_TIMEOUT_MS })
  await expectProjectContext(page, first)
  await expect.poll(() => requests.since(mark).length, { timeout: PAGE_TIMEOUT_MS }).toBeGreaterThan(0)
  expectOnlyProjectRequests(requests.since(mark), first, second)

  mark = requests.mark()
  await page.reload({ waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: '项目与工作空间', exact: true })).toBeVisible({ timeout: PAGE_TIMEOUT_MS })
  await expectProjectContext(page, first)
  await expect.poll(() => requests.since(mark).length, { timeout: PAGE_TIMEOUT_MS }).toBeGreaterThan(0)
  expectOnlyProjectRequests(requests.since(mark), first, second)

  // Keyboard: open, Escape closes and returns focus to the trigger.
  const trigger = page.getByRole('button', { name: '选择项目', exact: true })
  await trigger.focus()
  await page.keyboard.press('Enter')
  const selector = page.getByRole('dialog', { name: '项目选择器' })
  await expect(selector).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(selector).toBeHidden()
  await expect(trigger).toBeFocused()

  // Keyboard: search and Enter switch the project and drop the old project's object ids.
  mark = requests.mark()
  await page.keyboard.press('Enter')
  await expect(selector).toBeVisible()
  await page.getByRole('searchbox', { name: '搜索项目' }).fill(second.name)
  await page.keyboard.press('Enter')
  await expect(selector).toBeHidden()
  await expect(trigger).toBeFocused()
  await expectProjectContext(page, second)
  expect(new URL(page.url()).searchParams.has('releaseId')).toBe(false)
  await expect.poll(() => requests.since(mark).length, { timeout: PAGE_TIMEOUT_MS }).toBeGreaterThan(0)
  expectOnlyProjectRequests(requests.since(mark), second, first)

  // The home page names the same project and lists its in-progress sections.
  await page.goto(`/?projectId=${encodeURIComponent(first.id)}`, { waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { level: 1, name: first.name, exact: true })).toBeVisible({ timeout: PAGE_TIMEOUT_MS })
  await expectProjectContext(page, first)
  await expect(page.getByRole('heading', { name: '运行中的环境', exact: true })).toBeVisible({ timeout: PAGE_TIMEOUT_MS })

  // A link to a project the account cannot open shows recovery, not the previous project's page.
  const unavailableId = uuidv7()
  mark = requests.mark()
  await page.goto(`/researcher/workspaces?projectId=${encodeURIComponent(unavailableId)}`, { waitUntil: 'domcontentloaded' })
  const recovery = page.getByTestId('project-context-recovery')
  await expect(recovery.getByRole('heading', { name: '无法打开链接中的项目', exact: true })).toBeVisible({ timeout: PAGE_TIMEOUT_MS })
  await expect(recovery).toContainText('项目不存在或你无权访问。')
  await expect(page.getByRole('heading', { name: '项目与工作空间', exact: true })).toHaveCount(0)
  await expect(trigger.locator('.trigger-primary')).toHaveText('项目不可用')
  await expect(page).toHaveTitle(/^项目不可用 · /)
  const leaked = requests.since(mark).filter((path) => path.startsWith(`/api/v1/projects/${first.id}`) || path.startsWith(`/api/v1/projects/${second.id}`))
  expect(leaked).toEqual([])

  // The primary recovery action is focused and switches to an accessible project.
  const switchAction = recovery.getByRole('button', { name: /^切换到「.+」$/ })
  await expect(switchAction).toBeFocused()
  await page.keyboard.press('Enter')
  await expect(recovery).toBeHidden({ timeout: PAGE_TIMEOUT_MS })
  await expect(page.getByRole('heading', { name: '项目与工作空间', exact: true })).toBeVisible({ timeout: PAGE_TIMEOUT_MS })
  await expect.poll(() => new URL(page.url()).searchParams.get('projectId'), { timeout: PAGE_TIMEOUT_MS }).not.toBe(unavailableId)
})
