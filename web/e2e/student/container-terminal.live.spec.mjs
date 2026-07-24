import { expect, test } from '@playwright/test'
import { readFile, stat } from 'node:fs/promises'

const TERMINAL_PATH = '/student/environments'
const WRITE_ACK = 'LW_DEMO_WRITE_ACK'
const RECONNECT_ACK = 'LW_DEMO_RECONNECT_ACK'

function demoSlowMo() {
  const raw = process.env.LABWEAVER_DEMO_SLOW_MO_MS?.trim()
  if (!raw) return 0
  const value = Number(raw)
  if (!Number.isSafeInteger(value) || value < 0 || value > 5_000) {
    throw new Error('PW_TERMINAL_DEMO_SLOW_MO_INVALID')
  }
  return value
}

test.use({
  storageState: 'e2e/fixtures/empty.json',
  ignoreHTTPSErrors: true,
  locale: 'zh-CN',
  trace: 'off',
  screenshot: 'off',
  video: 'off',
  launchOptions: {
    slowMo: demoSlowMo(),
    args: [
      '--no-proxy-server',
      '--host-resolver-rules=MAP portal.labweaver.internal 127.0.0.1, MAP keycloak.labweaver.internal 127.0.0.1',
    ],
  },
})

function requiredEnvironment(name) {
  const value = process.env[name]?.trim()
  if (!value) throw new Error(`PW_TERMINAL_DEMO_INPUT_MISSING:${name}`)
  return value
}

async function readPassword(fileName) {
  const metadata = await stat(fileName)
  if (!metadata.isFile() || metadata.size < 1 || metadata.size > 4096) {
    throw new Error('PW_TERMINAL_DEMO_PASSWORD_FILE_INVALID')
  }
  const value = (await readFile(fileName, 'utf8')).replace(/[\r\n]+$/, '')
  if (!value || value.includes('\0') || value.includes('\r') || value.includes('\n')) {
    throw new Error('PW_TERMINAL_DEMO_PASSWORD_FILE_INVALID')
  }
  return value
}

async function login(page, baseURL) {
  const username = requiredEnvironment('LABWEAVER_STUDENT_USERNAME')
  const password = await readPassword(requiredEnvironment('LABWEAVER_STUDENT_PASSWORD_FILE'))
  await page.goto(TERMINAL_PATH, { waitUntil: 'domcontentloaded' })
  await expect(page).toHaveURL(/keycloak\.labweaver\.internal\/realms\/workloads\//)
  await page.locator('#username').fill(username)
  await page.locator('#password').fill(password)
  await Promise.all([
    page.waitForURL((url) => url.origin === new URL(baseURL).origin),
    page.locator('#kc-login').click(),
  ])
  await page.goto(TERMINAL_PATH, { waitUntil: 'domcontentloaded' })
  await expect(page).toHaveURL(new RegExp(`${TERMINAL_PATH.replaceAll('/', '\\/')}(?:[?#].*)?$`))
  await expect(page.getByRole('heading', { name: '环境控制台', exact: true }).first()).toBeVisible()
}

async function revokePreexistingGrants(page, environmentId) {
  return await page.evaluate(async (id) => {
    const list = await fetch(
      `/api/v1/environments/${id}/access-grants?state=active&includeTerminal=true`,
      { credentials: 'same-origin' },
    )
    if (!list.ok) throw new Error(`active grant lookup failed with HTTP ${list.status}`)
    const page = await list.json()
    const active = Array.isArray(page.items) ? page.items : []
    if (active.length === 0) return 0
    const csrfResponse = await fetch('/api/v1/auth/csrf', { credentials: 'same-origin' })
    if (!csrfResponse.ok) throw new Error(`CSRF lookup failed with HTTP ${csrfResponse.status}`)
    const { csrfToken } = await csrfResponse.json()
    if (typeof csrfToken !== 'string' || !csrfToken) throw new Error('invalid CSRF response')
    for (const grant of active) {
      const response = await fetch(`/api/v1/access-grants/${grant.id}/revoke`, {
        method: 'POST',
        credentials: 'same-origin',
        headers: {
          'Content-Type': 'application/json',
          'Idempotency-Key': crypto.randomUUID(),
          'If-Match': `"rev-${grant.revision}"`,
          'X-CSRF-Token': csrfToken,
        },
        body: JSON.stringify({ grantId: grant.id, reasonCode: 'demo_setup_revoke' }),
      })
      if (!response.ok) throw new Error(`preexisting grant revoke failed with HTTP ${response.status}`)
    }
    return active.length
  }, environmentId)
}

test('student demonstrates the access-bound Container terminal', async ({ page, baseURL }) => {
  test.slow()
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')
  const environmentId = requiredEnvironment('LABWEAVER_E2E_CONTAINER_ENVIRONMENT_ID')
  const terminalSockets = []
  const acknowledgements = { write: false, reconnect: false }
  const unexpectedConsoleErrors = []
  const httpFailures = []
  let authenticated = false
  let expectRevokedConnectionFailure = false

  page.on('console', (message) => {
    if (message.type() !== 'error') return
    const text = message.text()
    if (expectRevokedConnectionFailure && text.includes('/terminal')) return
    if (text.startsWith('Failed to load resource:')) return
    unexpectedConsoleErrors.push(text)
  })
  page.on('response', (response) => {
    if (response.status() < 400) return
    httpFailures.push({
      authenticated,
      path: new URL(response.url()).pathname,
      status: response.status(),
    })
  })
  page.on('websocket', (socket) => {
    if (!socket.url().includes('/terminal')) return
    terminalSockets.push(socket)
    socket.on('framereceived', ({ payload }) => {
      const value = Buffer.isBuffer(payload) ? payload.toString('utf8') : payload
      acknowledgements.write ||= value.includes(WRITE_ACK)
      acknowledgements.reconnect ||= value.includes(RECONNECT_ACK)
    })
  })

  await login(page, baseURL)
  authenticated = true
  await page.locator('#env-id-input').fill(environmentId)
  await page.getByRole('button', { name: '加载', exact: true }).click()
  await expect(page.getByText(environmentId, { exact: true })).toBeVisible()
  await expect(page.locator('.env-state--ready')).toBeVisible()
  const preexistingGrantsRevoked = await revokePreexistingGrants(page, environmentId)

  await page.getByRole('button', { name: '签发访问授权', exact: true }).click()
  await expect(page.locator('.grant-card .env-state--active')).toBeVisible({ timeout: 60_000 })
  await expect(page.getByRole('heading', { name: '容器终端', exact: true })).toBeVisible()

  await page.getByRole('button', { name: '连接', exact: true }).click()
  await expect(page.locator('.browser-terminal__status')).toContainText('已连接', { timeout: 30_000 })
  const viewport = page.getByLabel('容器交互终端')
  await viewport.click()
  await page.keyboard.type(
    `printf issue-131-connected > /workspace/.issue-131-demo && printf '${WRITE_ACK}\\n'`,
  )
  await page.keyboard.press('Enter')
  await expect.poll(() => acknowledgements.write).toBe(true)

  await page.getByRole('button', { name: '断开', exact: true }).click()
  await expect(page.locator('.browser-terminal__status')).toContainText('未连接')
  await page.getByRole('button', { name: '手动重连', exact: true }).click()
  await expect(page.locator('.browser-terminal__status')).toContainText('已连接', { timeout: 30_000 })
  await viewport.click()
  await page.keyboard.type(
    `test -s /workspace/.issue-131-demo && printf reconnected > /workspace/.issue-131-reconnect && printf '${RECONNECT_ACK}\\n'`,
  )
  await page.keyboard.press('Enter')
  await expect.poll(() => acknowledgements.reconnect).toBe(true)

  await page.getByRole('button', { name: '全屏', exact: true }).click()
  await expect(page.getByRole('button', { name: '退出全屏', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '退出全屏', exact: true }).click()

  const revokedTerminalUrl = terminalSockets.at(-1)?.url()
  if (!revokedTerminalUrl) throw new Error('PW_TERMINAL_DEMO_WEBSOCKET_NOT_OBSERVED')
  await page.getByRole('button', { name: '撤销授权', exact: true }).click()
  await expect(page.getByText('访问授权已撤销。', { exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: '容器终端', exact: true })).toHaveCount(0)

  expectRevokedConnectionFailure = true
  const deniedSocketPromise = page.waitForEvent(
    'websocket',
    (socket) => socket.url() === revokedTerminalUrl,
  )
  await page.evaluate((url) => {
    window.__labweaverRevokedTerminalProbe = new WebSocket(url, 'labweaver.terminal.v1')
  }, revokedTerminalUrl)
  const deniedSocket = await deniedSocketPromise
  await expect.poll(() => deniedSocket.isClosed(), { timeout: 30_000 }).toBe(true)
  expect(terminalSockets.length).toBeGreaterThanOrEqual(3)
  expect(unexpectedConsoleErrors).toEqual([])
  const terminalApiPrefixes = [
    '/api/v1/auth/csrf',
    `/api/v1/environments/${environmentId}/endpoints`,
    `/api/v1/environments/${environmentId}/access-grants`,
    '/api/v1/access-grants/',
  ]
  const terminalApiFailures = httpFailures.filter((failure) => (
    terminalApiPrefixes.some((prefix) => failure.path.startsWith(prefix))
    && (
      failure.status >= 500
      || failure.status === 404
      || (failure.status === 401 && failure.authenticated)
    )
  ))
  expect(terminalApiFailures).toEqual([])

  test.info().annotations.push({
    type: 'terminal-demo',
    description: JSON.stringify({
      authenticated: true,
      environmentReady: true,
      preexistingGrantsRevoked,
      terminalConnections: terminalSockets.length,
      writeAcknowledged: true,
      reconnectAcknowledged: true,
      revokedReconnectDenied: true,
      transcriptRetained: false,
      nonTerminalApiFailures: httpFailures.length - terminalApiFailures.length,
    }),
  })
})
