import { expect, test } from '@playwright/test'
import { mkdir, readFile, stat } from 'node:fs/promises'
import path from 'node:path'

const authDir = path.resolve('.auth')

const actors = Object.freeze([
  Object.freeze({
    role: 'teacher',
    usernameVariable: 'LABWEAVER_TEACHER_USERNAME',
    passwordFileVariable: 'LABWEAVER_TEACHER_PASSWORD_FILE',
    destination: path.join(authDir, 'teacher.json'),
    landingPath: '/teacher/materials',
    heading: '材料上传与 AgentRun',
  }),
  Object.freeze({
    role: 'student',
    usernameVariable: 'LABWEAVER_STUDENT_USERNAME',
    passwordFileVariable: 'LABWEAVER_STUDENT_PASSWORD_FILE',
    destination: path.join(authDir, 'student.json'),
    landingPath: '/student/environments',
    heading: '环境控制台',
  }),
])

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
    await page.goto(actor.landingPath)
    await expect(page.locator('#username')).toBeVisible()
    await page.locator('#username').fill(username)
    await page.locator('#password').fill(password)
    await Promise.all([
      page.waitForURL((url) => url.origin === new URL(baseURL).origin),
      page.locator('#kc-login').click(),
    ])
    await expect(page.getByRole('heading', { name: actor.heading })).toBeVisible()
    await expect(page).toHaveURL(new RegExp(`${actor.landingPath.replaceAll('/', '\\/')}(?:[?#].*)?$`))
    await context.storageState({ path: actor.destination })
  } catch (error) {
    throw new Error(`PW_KEYCLOAK_LOGIN_FAILED:${actor.role}`, { cause: error })
  } finally {
    await context.close()
  }
}

test('prepare real Keycloak teacher and student auth states', async ({ browser, baseURL }) => {
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')
  await mkdir(authDir, { recursive: true })
  for (const actor of actors) await authenticate({ browser, baseURL, actor })
})
