// Native Chromium navigation through real Access and disposable Keycloak.
// Product pages and frontend tests remain owned by Grok.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdtemp } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createInterface } from 'node:readline'

const require = createRequire(new URL('../../../web/package.json', import.meta.url))
const { chromium } = require('@playwright/test')
const base = process.argv[2]
assert.equal(new URL(base).hostname, 'localhost')
const screenshots = await mkdtemp(join(tmpdir(), 'labweaver-198-browser-'))
const replies = createInterface({ input: process.stdin })[Symbol.asyncIterator]()
let browser
const pageErrors = []
const httpErrors = []
const checked = []
const transactionCookies = async context => (await context.cookies(base)).filter(c => c.name.startsWith('__Host-labweaver_oidc_'))

async function pageFor(context) {
  const page = await context.newPage()
  page.on('pageerror', error => pageErrors.push(error.message))
  page.on('console', message => {
    if (message.type() === 'error') httpErrors.push(message.text())
  })
  return page
}

async function beginLogin(page, target) {
  await page.goto(`${base}/auth/login?return_to=${encodeURIComponent(target)}`)
  await page.locator('#username').waitFor({ state: 'visible' })
  assert.equal(await page.locator('#kc-form-login').count(), 1)
  const authorization = new URL(page.url())
  assert.equal(authorization.hostname, '127.0.0.1')
  assert.equal(authorization.searchParams.get('code_challenge_method'), 'S256')
  const state = authorization.searchParams.get('state')
  assert(state && state.length === 64)
  return state
}

async function authenticate(page, state) {
  const requested = page.waitForRequest(request => {
    const url = new URL(request.url())
    return request.isNavigationRequest() && url.origin === new URL(base).origin && url.pathname === '/auth/callback'
  })
  await page.locator('#username').fill('teacher')
  await page.locator('#password').fill('test-only-password')
  await page.locator('#kc-login').click()
  const callback = (await requested).url()
  assert(new URL(callback).searchParams.get('state') === state, 'callback state changed')
  return callback
}

async function rejectedNavigation(page, url, status, diagnostic) {
  const response = await page.goto(url)
  assert.equal(response.status(), status)
  assert.equal((await response.json()).diagnosticCode, diagnostic)
}

async function sessionStatus(page) {
  return page.evaluate(async () => (await fetch('/api/v1/auth/session')).status)
}

try {
  // Browser trusts only this disposable fixture via its test option. Rust
  // OIDC/service clients keep strict TLS and explicit fixture trust anchors.
  browser = await chromium.launch()
  const options = { ignoreHTTPSErrors: true, viewport: { width: 1280, height: 800 } }
  const initiating = await browser.newContext(options)
  const unrelated = await browser.newContext(options)
  const first = await pageFor(initiating)
  const second = await pageFor(initiating)
  const other = await pageFor(unrelated)
  const targets = ['/oidc-probe?project=first#active', '/oidc-probe?project=second']
  // Both native login navigations remain on their real Keycloak forms.
  const firstState = await beginLogin(first, targets[0])
  const secondState = await beginLogin(second, targets[1])
  await first.screenshot({ path: join(screenshots, 'keycloak-login.png') })
  const cookies = await transactionCookies(initiating)
  assert.equal(cookies.length, 2)
  assert(cookies.every(c => c.secure && c.httpOnly && c.sameSite === 'Lax' && c.path === '/' && !c.domain.startsWith('.')))
  checked.push('two independent Secure/HttpOnly/host-only/Lax transaction cookies')

  // Browser proof is checked before any code exchange. This rejection must
  // leave the pending transaction usable by its initiating native browser.
  await rejectedNavigation(other, `${base}/auth/callback?state=${firstState}&code=not-issued`, 401, 'LW_AUTH_OIDC_BROWSER_REJECTED')
  assert.equal(await sessionStatus(other), 401)
  checked.push('other browser rejected before consumption; initiating login can continue')
  const callbacks = []
  for (const [page, state, target] of [[first, firstState, targets[0]], [second, secondState, targets[1]]]) {
    callbacks.push(await authenticate(page, state))
    await page.waitForURL(url => url.href === `${base}${target}`)
    assert.equal(await page.title(), 'OIDC navigation probe')
    assert.equal(await page.locator('h1').textContent(), 'OIDC navigation probe')
    const authenticated = await sessionStatus(page)
    if (authenticated !== 200) {
      const names = (await initiating.cookies(base)).map(cookie => cookie.name)
      const diagnostic = await page.evaluate(async () => (await (await fetch('/api/v1/auth/session')).json()).diagnosticCode)
      throw new Error(`authenticated ${page === first ? 'first' : 'second'} tab failed: status=${authenticated}, diagnostic=${diagnostic}, cookie names=${names.join(',')}`)
    }
  }
  checked.push('both tabs complete native cross-site callbacks and preserve return query/fragment')
  assert.equal((await transactionCookies(initiating)).length, 0)
  await first.reload()
  assert.equal(await sessionStatus(first), 200)
  await first.screenshot({ path: join(screenshots, 'authenticated-destination.png') })
  checked.push('refresh retains authenticated BFF session')
  await rejectedNavigation(first, callbacks[0], 401, 'LW_AUTH_OIDC_BROWSER_REJECTED')
  await rejectedNavigation(other, callbacks[1], 401, 'LW_AUTH_OIDC_BROWSER_REJECTED')
  assert.equal(await sessionStatus(other), 401)
  checked.push('native replay and real callback copied to other browser fail')

  // The unrelated browser has no provider SSO session, so its pending real
  // login can expire at the database clock before submitting the login form.
  const expiredState = await beginLogin(other, '/oidc-probe?project=expired')
  process.stdout.write(`${JSON.stringify({ op: 'expire', stateSha256: createHash('sha256').update(expiredState).digest('hex') })}\n`)
  const reply = await replies.next()
  assert(!reply.done && JSON.parse(reply.value).ok)
  const expiredResponse = other.waitForResponse(response => {
    const url = new URL(response.url())
    return url.origin === new URL(base).origin && url.pathname === '/auth/callback'
  })
  await authenticate(other, expiredState)
  const rejected = await expiredResponse
  assert.equal(rejected.status(), 401)
  assert.equal((await rejected.json()).diagnosticCode, 'LW_AUTH_OIDC_STATE_REJECTED')
  assert.equal((await transactionCookies(unrelated)).length, 0)
  assert.equal(await sessionStatus(other), 401)
  assert.equal(await sessionStatus(first), 200)
  checked.push('expired real callback rejected and cleaned without revoking existing session')
  for (const target of ['//attacker.invalid/', '/\\attacker.invalid/', 'https://attacker.invalid/']) {
    await rejectedNavigation(first, `${base}/auth/login?return_to=${encodeURIComponent(target)}`, 400, 'LW_AUTH_OIDC_RETURN_TO_REJECTED')
  }
  checked.push('external and ambiguous return targets rejected by native navigation')

  const logoutUrl = await first.evaluate(async () => {
    const csrf = await (await fetch('/api/v1/auth/csrf')).json()
    const response = await fetch('/auth/logout', { method: 'POST', headers: { 'X-CSRF-Token': csrf.csrfToken } })
    if (response.status !== 200) throw new Error('BFF logout rejected')
    return (await response.json()).logoutUrl
  })
  await first.goto(logoutUrl)
  assert.equal(first.url(), `${base}/`)
  assert.equal(await sessionStatus(first), 401)
  assert.equal(await sessionStatus(second), 401)
  checked.push('browser CSRF logout, real provider navigation and shared-context revocation')
  assert.equal(pageErrors.length, 0, 'browser JavaScript runtime error')
  assert(httpErrors.every(message => /server responded with a status of (400|401)/.test(message)), 'unexpected browser console error')
  process.stdout.write(`${JSON.stringify({ op: 'complete', browserVersion: browser.version(), checked,
    expectedHttpErrorCount: httpErrors.length, screenshots })}\n`)
} catch (error) {
  const safe = String(error.message).replace(/([?&](?:code|state|session_state|nonce|id_token_hint|access_token|code_verifier)=)[^&\s]+/g, '$1[redacted]')
  process.stderr.write(`Chromium OIDC probe failed: ${safe}\n`)
  process.exitCode = 1
} finally {
  await browser?.close()
  process.stdin.destroy()
}
