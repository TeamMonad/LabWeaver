// Native Keycloak login -> actual Access session/CSRF -> actual Control/PG.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFile, mkdir } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, join, resolve } from 'node:path'
import { createInterface } from 'node:readline'
import { fileURLToPath, pathToFileURL } from 'node:url'

const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const webRoot = process.env.LABWEAVER_TEST_WEB_ROOT || resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const require = createRequire(join(webRoot, 'package.json'))
const { chromium, expect } = require('@playwright/test')
const { build } = await import(pathToFileURL(join(dirname(require.resolve('vite/package.json')), 'dist/node/index.js')))
const cipher = Buffer.from(fixture.ciphertext, 'base64url')
const digest = createHash('sha256').update(cipher).digest('hex')
const evidence = process.env.LABWEAVER_TEST_BROWSER_EVIDENCE_DIR || fixture.evidenceDirectory
const root = '/api/v1/admin/platform-configuration'
const checked = []
let browser
try {
  const sdkHtml = 'e2e/backend/platform-configuration-login.html'
  const target = fixture.surface === 'app' ? '/admin/platform-configuration' : `/${sdkHtml}`
  if (fixture.surface === 'app') await readFile(join(webRoot, 'src/views/admin/PlatformConfigurationView.vue'))
  await build({ root: webRoot, configFile: join(webRoot, 'vite.config.ts'), logLevel: 'silent',
    build: { outDir: fixture.webDist, emptyOutDir: false,
      ...(fixture.surface === 'sdk' ? { rollupOptions: { input: join(webRoot, sdkHtml) } } : {}) } })
  process.stdout.write(`${JSON.stringify({ op: 'web_ready' })}\n`)
  const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]()
  assert(JSON.parse((await lines.next()).value).ok)
  await mkdir(evidence, { recursive: true })
  browser = await chromium.launch({ ...(process.env.LABWEAVER_TEST_CHROMIUM ? { executablePath: process.env.LABWEAVER_TEST_CHROMIUM } : {}) })
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1280, height: 800 } })
  const page = await context.newPage()
  let phase = 'anonymous'
  const errors = [], warnings = [], rejected = []
  page.on('pageerror', e => errors.push(e.message))
  page.on('console', m => { if (['error', 'warning'].includes(m.type())) warnings.push(m.text()) })
  page.on('response', r => { if (r.status() >= 400) rejected.push({ path: new URL(r.url()).pathname, status: r.status(), phase }) })
  const login = async (tab, username, destination) => {
    await tab.goto(fixture.surface === 'app' && username === 'admin'
      ? `${fixture.base}${destination}`
      : `${fixture.base}/auth/login?return_to=${encodeURIComponent(destination)}`)
    await expect(tab.locator('#kc-form-login')).toBeVisible()
    assert.equal(new URL(tab.url()).hostname, '127.0.0.1')
    assert.equal(new URL(tab.url()).searchParams.get('code_challenge_method'), 'S256')
    await tab.screenshot({ path: join(evidence, `${fixture.surface}-${username}-keycloak.png`) })
    const callback = tab.waitForRequest(r => r.isNavigationRequest() && new URL(r.url()).origin === new URL(fixture.base).origin && new URL(r.url()).pathname === '/auth/callback')
    await tab.locator('#username').fill(username)
    await tab.locator('#password').fill('test-only-password')
    await tab.locator('#kc-login').click()
    await callback
    await tab.waitForURL(`${fixture.base}${destination}`)
  }
  await login(page, 'admin', target)
  const session = await page.evaluate(async () => (await fetch('/api/v1/auth/session')).json())
  assert(session.actor.roles.includes('platform_admin'))
  phase = 'authenticated'
  const cookie = (await context.cookies(fixture.base)).find(c => c.name === '__Host-labweaver_session')
  assert(cookie?.secure && cookie.httpOnly && cookie.sameSite === 'Lax' && cookie.path === '/')
  assert(!(await page.evaluate(() => document.cookie)).includes('__Host-labweaver_session'))
  assert(!(await context.cookies(fixture.base)).some(c => c.name.startsWith('__Host-labweaver_oidc_')))
  checked.push('native Keycloak login/PKCE callback and Secure HttpOnly BFF session')
  const initial = await page.evaluate(async () => (await fetch('/api/v1/admin/platform-configuration')).json())
  assert.equal(initial.revision, 0)
  for (const csrf of [null, 'wrong-synthetic-token']) {
    const denied = await page.evaluate(async ({ csrf, defaults }) => {
      const r = await fetch('/api/v1/admin/platform-configuration', { method: 'PATCH',
        headers: { 'Content-Type': 'application/json', 'If-Match': '"rev-0"', 'Idempotency-Key': 'negative-csrf', ...(csrf ? { 'X-CSRF-Token': csrf } : {}) },
        body: JSON.stringify({ expectedRevision: 0, configuration: defaults }) })
      return { status: r.status, diagnostic: (await r.json()).diagnosticCode }
    }, { csrf, defaults: initial.defaults })
    assert.deepEqual(denied, { status: 403, diagnostic: 'LW_AUTH_CSRF_REJECTED' })
  }
  const csrf = await (await context.request.get(`${fixture.base}/api/v1/auth/csrf`)).json()
  const originDenied = await context.request.patch(`${fixture.base}${root}`, { headers: {
    Origin: 'https://attacker.invalid', 'X-CSRF-Token': csrf.csrfToken, 'If-Match': '"rev-0"', 'Idempotency-Key': 'negative-origin' },
    data: { expectedRevision: 0, configuration: initial.defaults } })
  assert.equal(originDenied.status(), 403)
  assert.equal((await originDenied.json()).diagnosticCode, 'LW_AUTH_CSRF_REJECTED')
  assert.equal(await page.evaluate(async () => (await (await fetch('/api/v1/admin/platform-configuration')).json()).revision), 0)
  checked.push('missing/wrong CSRF and foreign Origin rejected without writing')
  let requested
  if (fixture.surface === 'sdk') {
    await page.waitForFunction(() => window.configurationBffSdk)
    requested = await page.evaluate(async ({ bytes, digest }) => {
      const sdk = window.configurationBffSdk
      const initial = await sdk.getPlatformConfiguration({ throwOnError: true })
      const saved = await sdk.updatePlatformConfiguration({ throwOnError: true,
        headers: { 'If-Match': initial.headers.etag, 'Idempotency-Key': 'bff-save' },
        body: { expectedRevision: initial.data.revision, configuration: initial.data.defaults } })
      for (const csrf of [null, 'wrong-synthetic-token']) {
        const denied = await fetch(`/api/v1/admin/platform-configuration/versions/${saved.data.desired.revision}/bundle?expectedRevision=${saved.data.revision}&configurationSha256=${saved.data.desired.configurationSha256}`, {
          method: 'POST', headers: { 'Content-Type': 'application/octet-stream', 'If-Match': saved.headers.etag,
            'Idempotency-Key': 'negative-bundle-csrf', ...(csrf ? { 'X-CSRF-Token': csrf } : {}) },
          body: new File([new Uint8Array(bytes)], 'synthetic.age', { type: 'application/octet-stream' }),
        })
        if (denied.status !== 403 || (await denied.json()).diagnosticCode !== 'LW_AUTH_CSRF_REJECTED') {
          throw new Error('Bundle upload bypassed CSRF')
        }
      }
      const registered = await sdk.registerPlatformConfigurationBundle({ throwOnError: true,
        path: { revision: saved.data.desired.revision }, query: { expectedRevision: saved.data.revision, configurationSha256: saved.data.desired.configurationSha256 },
        headers: { 'If-Match': saved.headers.etag, 'Idempotency-Key': 'bff-register' },
        body: new File([new Uint8Array(bytes)], 'synthetic.age', { type: 'application/octet-stream' }) })
      if (registered.data.registeredBundle.encryptedBundleSha256 !== digest) throw new Error('registration digest mismatch')
      const apply = await sdk.applyPlatformConfiguration({ throwOnError: true,
        headers: { 'If-Match': registered.headers.etag, 'Idempotency-Key': 'bff-apply' },
        body: { expectedRevision: registered.data.revision, desiredRevision: registered.data.desired.revision } })
      if (apply.status !== 202) throw new Error('apply status mismatch')
      return apply.data
    }, { bytes: [...cipher], digest })
  } else {
    await expect(page.getByRole('heading', { name: '平台配置', exact: true })).toBeVisible()
    assert.equal(await page.title(), '平台配置 · LabWeaver')
    await expect(page.getByTestId('pc-apply')).toBeDisabled()
    let r = page.waitForResponse(x => x.request().method() === 'PATCH' && new URL(x.url()).pathname === root)
    await page.getByTestId('pc-save').click()
    const savedResponse = await r
    assert.equal(savedResponse.status(), 200)
    const saved = await savedResponse.json()
    await page.evaluate(async ({ saved, bytes }) => {
      for (const csrf of [null, 'wrong-synthetic-token']) {
        const r = await fetch(`/api/v1/admin/platform-configuration/versions/${saved.desired.revision}/bundle?expectedRevision=${saved.revision}&configurationSha256=${saved.desired.configurationSha256}`, {
          method: 'POST', headers: { 'Content-Type': 'application/octet-stream', 'If-Match': `"rev-${saved.revision}"`,
            'Idempotency-Key': 'negative-bundle-csrf', ...(csrf ? { 'X-CSRF-Token': csrf } : {}) },
          body: new File([new Uint8Array(bytes)], 'synthetic.age', { type: 'application/octet-stream' }),
        })
        if (r.status !== 403 || (await r.json()).diagnosticCode !== 'LW_AUTH_CSRF_REJECTED') throw new Error('Bundle upload bypassed CSRF')
      }
    }, { saved, bytes: [...cipher] })
    await page.getByTestId('pc-bundle-file').setInputFiles({ name: 'synthetic.age', mimeType: 'application/octet-stream', buffer: cipher })
    r = page.waitForResponse(x => x.request().method() === 'POST' && new URL(x.url()).pathname.endsWith('/bundle'))
    await page.getByTestId('pc-register').click()
    const registered = await r
    assert.equal(registered.status(), 200)
    assert.equal((await registered.json()).registeredBundle.encryptedBundleSha256, digest)
    await expect(page.getByTestId('pc-registered-bundle')).toContainText(digest)
    await expect(page.getByTestId('pc-applied-state')).toHaveText('尚无验证结果')
    await page.getByRole('heading', { name: '当前事实', exact: true }).scrollIntoViewIfNeeded()
    await page.screenshot({ path: join(evidence, 'app-registered.png'), fullPage: true })
    await page.getByTestId('pc-apply').click()
    r = page.waitForResponse(x => x.request().method() === 'POST' && new URL(x.url()).pathname.endsWith('/applies'))
    await page.getByTestId('pc-apply-confirm').click()
    const applied = await r
    assert.equal(applied.status(), 202)
    assert.deepEqual(Object.keys(applied.request().postDataJSON()).sort(), ['desiredRevision', 'expectedRevision'])
    requested = await applied.json()
  }
  assert.equal(requested.latestApply.stage, 'requested')
  assert.equal(requested.registeredBundle.encryptedBundleSha256, digest)
  assert.equal(requested.applied, null)
  checked.push(`${fixture.surface}: Bundle POST CSRF rejection, actual BFF writes, File registration and 202 without applied`)
  await page.reload()
  assert.equal(await page.evaluate(async () => (await (await fetch('/api/v1/admin/platform-configuration')).json()).revision), requested.revision)
  await page.screenshot({ path: join(evidence, `${fixture.surface}-authenticated.png`), fullPage: true })
  if (fixture.surface === 'app') {
    await expect(page.getByTestId('pc-registered-bundle')).toContainText(digest)
    await page.setViewportSize({ width: 390, height: 844 })
    await page.getByRole('heading', { name: '平台配置', exact: true }).scrollIntoViewIfNeeded()
    await page.screenshot({ path: join(evidence, 'app-mobile.png'), fullPage: true })
    await page.setViewportSize({ width: 1280, height: 800 })
  }
  const student = await browser.newContext({ ignoreHTTPSErrors: true })
  const studentPage = await student.newPage()
  await login(studentPage, 'student', '/oidc-probe')
  assert.equal(await studentPage.evaluate(async () => (await fetch('/api/v1/admin/platform-configuration')).status), 403)
  checked.push('real non-admin Keycloak identity rejected by Access/Control')
  const logout = await page.evaluate(async () => {
    const csrf = await (await fetch('/api/v1/auth/csrf')).json()
    const r = await fetch('/auth/logout', { method: 'POST', headers: { 'X-CSRF-Token': csrf.csrfToken } })
    if (r.status !== 200) throw new Error('BFF logout rejected')
    return (await r.json()).logoutUrl
  })
  phase = 'logout'
  await page.goto(logout)
  const revoked = await page.evaluate(async () => ({ session: (await fetch('/api/v1/auth/session')).status,
    configuration: (await fetch('/api/v1/admin/platform-configuration')).status }))
  assert.deepEqual(revoked, { session: 401, configuration: 401 })
  checked.push('CSRF logout and native provider logout revoke subsequent reads')
  assert.deepEqual(errors, [])
  assert.equal(await page.locator('vite-error-overlay').count(), 0)
  assert(rejected.every(({ path, status, phase }) =>
    ([401, 403].includes(status) && (path.startsWith(root) || path === '/api/v1/auth/session')) ||
    (fixture.surface === 'app' && phase === 'anonymous' && path === '/api/v1/projects' && status === 401)),
    `unexpected failed browser resource: ${JSON.stringify(rejected)}`)
  assert(warnings.every(m => /^Failed to load resource: the server responded with a status of (401|403)\b/.test(m)), `unexpected console warning: ${warnings.slice(0, 3).join('; ')}`)
  process.stdout.write(`${JSON.stringify({ op: 'complete', surface: fixture.surface, checked, browserVersion: browser.version(), evidence })}\n`)
} catch (error) {
  const safe = String(error.message).replace(/([?&](?:code|state|session_state|nonce|id_token_hint|access_token)=)[^&\s]+/g, '$1[redacted]')
  process.stderr.write(`Configuration BFF probe failed: ${safe}\n`)
  process.exitCode = 1
} finally {
  await browser?.close()
  process.stdin.destroy()
}
