import { expect, test } from '@playwright/test'
import { mkdir, readFile, stat } from 'node:fs/promises'
import path from 'node:path'

const authDir = path.resolve(process.env.LABWEAVER_AUTH_DIR || '.auth')

const actors = Object.freeze([
  Object.freeze({
    role: 'teacher',
    usernameVariable: 'LABWEAVER_TEACHER_USERNAME',
    passwordFileVariable: 'LABWEAVER_TEACHER_PASSWORD_FILE',
    destination: path.join(authDir, 'teacher.json'),
    landingPath: '/teacher/materials',
    entryLabel: '创建与生成实验',
    heading: '材料上传与 AgentRun',
  }),
  Object.freeze({
    role: 'student',
    usernameVariable: 'LABWEAVER_STUDENT_USERNAME',
    passwordFileVariable: 'LABWEAVER_STUDENT_PASSWORD_FILE',
    destination: path.join(authDir, 'student.json'),
    landingPath: '/student/environments',
    entryLabel: '环境控制台',
    heading: '项目环境控制台',
  }),
  Object.freeze({
    role: 'platform-admin',
    usernameVariable: 'LABWEAVER_PLATFORM_ADMIN_USERNAME',
    passwordFileVariable: 'LABWEAVER_PLATFORM_ADMIN_PASSWORD_FILE',
    destination: path.join(authDir, 'platform-admin.json'),
    landingPath: '/admin/resource-approval',
    entryLabel: '资源审批',
    heading: '资源审批与资源使用授权管理',
  }),
])

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function requiredEnvironment(name) {
  const value = process.env[name]?.trim()
  if (!value) throw new Error(`PW_AUTH_CONFIGURATION_MISSING:${name}`)
  return value
}

async function readPassword(fileName) {
  const metadata = await stat(fileName)
  if (!metadata.isFile() || metadata.size < 1 || metadata.size > 4096) {
    throw new Error('PW_AUTH_PASSWORD_FILE_INVALID')
  }
  const value = (await readFile(fileName, 'utf8')).replace(/[\r\n]+$/, '')
  if (!value || value.includes('\0') || value.includes('\r') || value.includes('\n')) {
    throw new Error('PW_AUTH_PASSWORD_FILE_INVALID')
  }
  return value
}

async function authenticate({ browser, baseURL, actor }) {
  const username = requiredEnvironment(actor.usernameVariable)
  const password = await readPassword(requiredEnvironment(actor.passwordFileVariable))
  const context = await browser.newContext({ baseURL })
  const page = await context.newPage()
  try {
    // Start from the public home and use its visible login control. This keeps
    // the setup journey aligned with the path a new user can actually follow.
    await page.goto('/', {
      waitUntil: 'domcontentloaded',
    })
    await expect(page.getByRole('heading', { name: '欢迎进入 LabWeaver', exact: true })).toBeVisible()
    const login = page.getByRole('button', { name: '登录', exact: true }).first()
    await expect(login).toBeVisible()
    await login.click({ noWaitAfter: true })
    await expect(page.locator('#username')).toBeVisible()
    await page.locator('#username').fill(username)
    await page.locator('#password').fill(password)
    await Promise.all([
      page.waitForURL((url) => url.origin === new URL(baseURL).origin, {
        waitUntil: 'domcontentloaded',
      }),
      page.locator('#kc-login').click({ noWaitAfter: true }),
    ])
    // When authentication returns to the task home, follow the actor's
    // authorized task card before asserting the protected landing page. The
    // drawer can be collapsed into a rail whose accessible labels include the
    // group name, so the public home card is the stable user-facing entry.
    if (!new URL(page.url()).pathname.startsWith(actor.landingPath)) {
      await expect(page.getByRole('heading', { name: '欢迎进入 LabWeaver', exact: true })).toBeVisible()
      const taskLink = page.locator('.task-grid').getByRole('link', {
        name: new RegExp(`^${escapeRegExp(actor.entryLabel)}`),
      })
      await expect(taskLink).toHaveCount(1, { timeout: 60_000 })
      await taskLink.click()
    }
    await expect(page.getByRole('heading', { name: actor.heading }).first()).toBeVisible()
    await expect(page).toHaveURL(new RegExp(`${actor.landingPath.replaceAll('/', '\\/')}(?:[?#].*)?$`))
    await context.storageState({ path: actor.destination })
  } catch (error) {
    throw new Error(`PW_KEYCLOAK_LOGIN_FAILED:${actor.role}`, { cause: error })
  } finally {
    await context.close()
  }
}

for (const actor of actors) {
  test(`prepare real Keycloak ${actor.role} auth state`, async ({ browser, baseURL }) => {
    if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')
    await mkdir(authDir, { recursive: true })
    await authenticate({ browser, baseURL, actor })
  })
}
