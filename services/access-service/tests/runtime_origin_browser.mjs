// Native browser proof against the real Access and Environment HTTP authorities.
import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { createInterface } from 'node:readline'

const require = createRequire(new URL('../../../web/package.json', import.meta.url))
const { chromium } = require('@playwright/test')
const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]()
const { portal, runtime, grant, session } = JSON.parse((await lines.next()).value)
const browser = await chromium.launch(process.env.LABWEAVER_TEST_CHROMIUM
  ? { executablePath: process.env.LABWEAVER_TEST_CHROMIUM } : {})
try {
  // Only the browser trusts this disposable TLS fixture; service clients verify its CA.
  const context = await browser.newContext({ ignoreHTTPSErrors: true })
  await context.addCookies([{ name: '__Host-labweaver_session', value: session,
    url: portal, httpOnly: true, secure: true, sameSite: 'Lax' }])
  const authenticated = await context.request.get(`${portal}/api/v1/auth/session`)
  assert.equal(authenticated.status(), 200, await authenticated.text())
  const page = await context.newPage()
  const runtimeRequests = []
  page.on('request', request => {
    if (new URL(request.url()).origin === runtime) runtimeRequests.push(request)
  })
  const navigation = await page.goto(`${portal}/connect/${grant}/`)
  assert.equal(navigation.status(), 200, await navigation.text())
  await page.waitForURL(`${runtime}/connect/${grant}/`)
  assert.equal(await page.title(), 'Untrusted runtime')
  assert.equal(await page.evaluate(() => window.runtimeScriptRan), true)
  const runtimeCookies = await context.cookies(`${runtime}/connect/${grant}/`)
  assert(runtimeCookies.some(cookie => cookie.name === '__Secure-labweaver_runtime'))
  assert(runtimeCookies.every(cookie => cookie.name !== '__Host-labweaver_session'))
  assert.equal(await page.evaluate(() => document.cookie), '')
  for (const request of runtimeRequests) {
    const headers = await request.allHeaders()
    assert(!headers.cookie?.includes('__Host-labweaver_session='))
  }
  // Chromium deliberately does not expose the rejected CORS response to the page.
  // The HTTP regression separately asserts Access returns 403 without allow-origin.
  const blocked = page.waitForEvent('requestfailed', { predicate: request => request.url() === `${portal}/api/v1/auth/csrf` })
  const stolen = await page.evaluate(async portal => {
    try {
      const response = await fetch(`${portal}/api/v1/auth/csrf`, { credentials: 'include' })
      return await response.text()
    } catch { return null }
  }, portal)
  assert.equal(stolen, null, 'untrusted script must not read a portal CSRF token')
  assert((await blocked).failure(), 'the browser must reject the cross-origin API fetch')
  const localApi = await page.evaluate(async () => (await fetch('/api/v1/auth/session')).status)
  assert.equal(localApi, 403)
  const posted = await page.evaluate(async () => {
    const response = await fetch(location.href, { method: 'POST', body: 'runtime-browser-post' })
    return { status: response.status, text: await response.text() }
  })
  assert.deepEqual(posted, { status: 200, text: 'runtime-browser-post' })
  console.log('PASS: native handoff, HttpOnly host-only cookies, no BFF cookie on runtime, runtime script denied portal CSRF/API, runtime POST without portal CSRF')
} finally {
  await browser.close()
}
