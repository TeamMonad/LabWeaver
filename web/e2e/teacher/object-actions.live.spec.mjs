import { expect, test } from '@playwright/test'
import { AUTH_STATE, createProjectByUi, expectJson, selectProjectByUi, uuidv7 } from '../support/live.mjs'
import { addProjectStudentByUi } from '../support/real-experiment.mjs'

/**
 * Object action journey (#199): a project responsibility, not the platform
 * identity, decides what the console offers. A student added as a viewer reads
 * the project and its members but gets no edit, archive, member or Work entry,
 * and the page says which responsibility is missing. The server answers the
 * same question through available-actions, and refuses a direct write.
 */

const PAGE_TIMEOUT_MS = 60_000

test('a viewer member sees the project read-only with the missing responsibility explained', async ({ page, browser, baseURL }) => {
  const project = await createProjectByUi(page, `对象动作 ${uuidv7().slice(-8)}`)
  await addProjectStudentByUi(page, project.id, undefined, 'viewer')

  const studentContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.student })
  try {
    const studentPage = await studentContext.newPage()
    const actions = await expectJson(
      await studentPage.request.get(`/api/v1/projects/${encodeURIComponent(project.id)}/available-actions`),
      'OBJECT_ACTIONS_READ_FAILED',
    )
    expect(actions).toMatchObject({ projectId: project.id, actions: ['browse'] })

    await studentPage.goto(`/researcher/workspaces?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
    await selectProjectByUi(studentPage, project.id)
    await expect(studentPage.getByRole('heading', { name: '项目成员', exact: true })).toBeVisible({ timeout: PAGE_TIMEOUT_MS })
    await expect(studentPage.getByTestId('edit-permission-hint')).toContainText('你在此项目的职责不包含“编辑”', { timeout: PAGE_TIMEOUT_MS })
    await expect(studentPage.getByTestId('members-permission-hint')).toContainText('不包含“管理成员”')
    await expect(studentPage.getByRole('button', { name: '保存项目', exact: true })).toHaveCount(0)
    await expect(studentPage.getByRole('button', { name: '归档项目', exact: true })).toHaveCount(0)
    await expect(studentPage.locator('form.member-form')).toHaveCount(0)
    await expect(studentPage.getByRole('link', { name: '新建 Work' })).toHaveCount(0)
    await expect(studentPage.locator('.member-row').filter({ hasText: '项目职责：查看者' })).toHaveCount(1)

    // Hiding the button is not the authorization: the server refuses the write itself.
    const denied = await studentPage.request.fetch(`/api/v1/projects/${encodeURIComponent(project.id)}`, {
      method: 'PATCH',
      headers: { 'If-Match': `"rev-${project.revision}"`, 'Idempotency-Key': uuidv7(), 'Content-Type': 'application/json' },
      data: { name: `${project.name} 越权`, expectedRevision: project.revision },
    })
    expect(denied.status()).toBe(403)
  } finally {
    await studentContext.close()
  }
})
