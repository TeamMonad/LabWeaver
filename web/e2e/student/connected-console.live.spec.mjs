import { readFile, unlink, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { expect, test } from '@playwright/test'

const CASES = ['positive', 'revoke', 'expiry', 'stop', 'delete', 'control-channel-loss']

function required(name) {
  const value = process.env[name]?.trim()
  if (!value) throw new Error(`PW_RUNTIME_INPUT_MISSING:${name}`)
  return value
}

async function caseIds(name) {
  const value = JSON.parse(await readFile(required(name), 'utf8'))
  for (const id of CASES) {
    if (typeof value[id] !== 'string' || !value[id]) throw new Error(`PW_CONSOLE_CASE_FILE_INVALID:${name}`)
  }
  return value
}

async function load(page, environmentId, runtimeLabel) {
  await page.goto('/student/environments', { waitUntil: 'domcontentloaded' })
  await page.getByLabel('环境 ID').fill(environmentId)
  await page.getByRole('button', { name: '加载', exact: true }).click()
  await expect(page.locator('.env-id')).toHaveText(environmentId)
  await expect(page.locator('.env-runtime')).toHaveText(runtimeLabel)
  await expect(page.locator('.env-state')).toHaveText('运行中')
}

async function ensureGrant(page) {
  const button = page.getByRole('button', { name: '签发访问授权', exact: true })
  if (await button.isVisible()) await button.click()
  await expect(page.locator('.grant-card').getByText('生效中', { exact: true })).toBeVisible()
}

async function openConsole(page, kind) {
  const label = kind === 'xterm' ? '打开终端' : '打开图形控制台'
  const [response] = await Promise.all([
    page.waitForResponse((candidate) => candidate.request().method() === 'POST'
      && candidate.url().includes('/console-capabilities') && candidate.ok()),
    page.getByRole('button', { name: label, exact: true }).click(),
  ])
  if (kind === 'xterm') await expect(page.locator('.xterm-helper-textarea')).toBeVisible()
  else await expect(page.getByTestId('novnc-connection-state')).toHaveText('图形控制台已连接')
  return response.json()
}

async function createShortGrant(page, environmentId) {
  const result = await page.evaluate(async ({ environmentId }) => {
    const read = async (url) => {
      const response = await fetch(url)
      if (!response.ok) throw new Error(`HTTP_${response.status}`)
      return response.json()
    }
    const environment = await read(`/api/v1/environments/${environmentId}`)
    const endpoints = await read(`/api/v1/environments/${environmentId}/endpoints`)
    const csrf = await read('/api/v1/auth/csrf')
    const response = await fetch(`/api/v1/environments/${environmentId}/access-grants`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Idempotency-Key': crypto.randomUUID(),
        'X-CSRF-Token': csrf.csrfToken,
      },
      body: JSON.stringify({
        courseId: environment.courseId,
        environmentId,
        environmentRevision: environment.revision,
        endpointIds: endpoints.items.map((endpoint) => endpoint.id),
        expiresAt: new Date(Date.now() + 30_000).toISOString(),
      }),
    })
    if (!response.ok) throw new Error(`HTTP_${response.status}`)
    return response.json()
  }, { environmentId })
  await expect.poll(async () => {
    const response = await page.request.get(`/api/v1/access-grants/${result.id}`)
    return (await response.json()).state
  }).toBe('active')
  await page.reload({ waitUntil: 'domcontentloaded' })
}

async function staleLocatorIsDenied(page, capability) {
  const denied = await page.evaluate(async ({ locator, protocol }) => new Promise((resolve) => {
    const url = new URL(locator, window.location.origin)
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
    const socket = new WebSocket(url, protocol)
    socket.addEventListener('open', () => { socket.close(); resolve(false) })
    socket.addEventListener('close', () => resolve(true))
    socket.addEventListener('error', () => resolve(true))
  }), { locator: capability.connectionLocator, protocol: capability.websocketSubprotocol })
  expect(denied).toBe(true)
}

function consoleMatrix({ runtime, label, kind, input }) {
  test.describe.serial(`${runtime} connected console`, () => {
    let ids
    test.beforeAll(async () => { ids = await caseIds(input) })

    for (const lifecycle of CASES) {
      test(`${lifecycle} fails closed with an isolated environment`, async ({ page }) => {
        test.skip(runtime === 'kubevirt' && process.env.LABWEAVER_E2E_SKIP_VM === 'true')
        const environmentId = ids[lifecycle]
        await load(page, environmentId, label)
        if (lifecycle === 'expiry') await createShortGrant(page, environmentId)
        else await ensureGrant(page)

        const capability = await openConsole(page, kind)
        if (kind === 'xterm') {
          const marker = `LW_CONSOLE_PROBE_${crypto.randomUUID().replaceAll('-', '')}`
          await page.locator('.xterm-helper-textarea').focus()
          await page.keyboard.type(`printf '${marker}\\n'`)
          await page.keyboard.press('Enter')
          await expect(page.locator('.xterm-screen')).toContainText(marker)
          await page.getByRole('button', { name: '全屏', exact: true }).click()
          await expect.poll(() => page.locator('.console-body').evaluate((el) => el.clientWidth)).toBeGreaterThan(0)
          await page.evaluate(() => document.exitFullscreen())
        }
        expect(capability).toBeTruthy()
        await staleLocatorIsDenied(page, capability)

        if (lifecycle === 'positive') {
          await page.getByRole('button', { name: '断开', exact: true }).click()
          await expect(page.getByRole('button', {
            name: kind === 'xterm' ? '打开终端' : '打开图形控制台', exact: true,
          })).toBeVisible()
          await openConsole(page, kind)
        } else if (lifecycle === 'revoke') {
          await page.getByRole('button', { name: '撤销授权', exact: true }).click()
          await expect(page.getByText('ACCESS_GRANT_REVOKED')).toBeVisible()
        } else if (lifecycle === 'expiry') {
          await expect(page.getByText(/CONSOLE_|会话已断开|连接失败/)).toBeVisible({ timeout: 60_000 })
        } else if (lifecycle === 'stop') {
          await page.getByRole('button', { name: '停止', exact: true }).click()
          await expect(page.locator('.env-state')).not.toHaveText('运行中', { timeout: 60_000 })
        } else if (lifecycle === 'delete') {
          await page.getByRole('button', { name: '删除', exact: true }).click()
          await page.getByRole('alertdialog').getByRole('button', { name: '删除', exact: true }).click()
          await expect(page.getByText(/ENVIRONMENT_|不存在|not found/i)).toBeVisible({ timeout: 60_000 })
        } else {
          const root = required('LABWEAVER_E2E_CONTROL_CHANNEL_COORDINATION_DIR')
          const prefix = `${runtime}-${environmentId}`
          await writeFile(path.join(root, `${prefix}.ready`), '', { flag: 'wx' })
          await expect.poll(async () => readFile(path.join(root, `${prefix}.applied`), 'utf8').then(() => true).catch(() => false), { timeout: 60_000 }).toBe(true)
          await expect(page.getByText(/CONSOLE_|会话已断开|连接失败/)).toBeVisible({ timeout: 60_000 })
          await writeFile(path.join(root, `${prefix}.observed`), '', { flag: 'wx' })
          await expect.poll(async () => readFile(path.join(root, `${prefix}.restored`), 'utf8').then(() => true).catch(() => false), { timeout: 60_000 }).toBe(true)
          await openConsole(page, kind)
          await Promise.all(['ready', 'applied', 'observed', 'restored'].map(
            (phase) => unlink(path.join(root, `${prefix}.${phase}`)),
          ))
        }
      })
    }
  })
}

consoleMatrix({ runtime: 'container', label: '容器', kind: 'xterm', input: 'LABWEAVER_E2E_CONTAINER_CONSOLE_CASES_FILE' })
consoleMatrix({ runtime: 'kubevirt', label: '虚拟机', kind: 'novnc', input: 'LABWEAVER_E2E_KUBEVIRT_CONSOLE_CASES_FILE' })
