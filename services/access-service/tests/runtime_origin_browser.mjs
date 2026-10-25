// Native browser proof against the real Access and Environment HTTP authorities.
import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { createInterface } from 'node:readline'

const require = createRequire(new URL('../../../web/package.json', import.meta.url))
const { chromium } = require('@playwright/test')
const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]()
const { portal, runtime, grant, runtimeB, grantB, session } = JSON.parse((await lines.next()).value)
const browser = await chromium.launch({
  args: ['--no-proxy-server'],
  ...(process.env.LABWEAVER_TEST_CHROMIUM ? { executablePath: process.env.LABWEAVER_TEST_CHROMIUM } : {}),
})
try {
  // Only this disposable browser context trusts the test certificate. Service clients verify its CA.
  const context = await browser.newContext({ ignoreHTTPSErrors: true })
  await context.addCookies([{ name: '__Host-labweaver_session', value: session,
    url: portal, httpOnly: true, secure: true, sameSite: 'Lax' }])
  const page = await context.newPage()
  const authenticated = await page.goto(`${portal}/api/v1/auth/session`)
  assert.equal(authenticated.status(), 200, await authenticated.text())
  const runtimeRequests = []
  const requestTargets = []
  context.on('request', request => {
    const url = new URL(request.url())
    requestTargets.push(url.pathname + url.search)
    if (url.hostname.endsWith('.runtime.localhost')) runtimeRequests.push(request)
  })
  async function openGrant(page, id, origin) {
    const navigation = await page.goto(`${portal}/connect/${id}/`)
    assert.equal(navigation.status(), 200, await navigation.text())
    await page.waitForURL(`${origin}/connect/${id}/`)
    assert.equal(await page.title(), 'Untrusted runtime')
    assert.equal(await page.evaluate(() => window.runtimeScriptRan), true)
    assert.equal(await page.evaluate(() => document.cookie), '')
  }
  // Both grants are live, in different real projects/environments, with credentials in the same browser.
  const second = await context.newPage()
  await openGrant(second, grantB, runtimeB)
  await openGrant(page, grant, runtime)
  const aCookies = await context.cookies(`${runtime}/connect/${grant}/`)
  const bCookies = await context.cookies(`${runtimeB}/connect/${grantB}/`)
  const a = aCookies.find(cookie => cookie.name === '__Secure-labweaver_runtime')
  const b = bCookies.find(cookie => cookie.name === '__Secure-labweaver_runtime')
  assert(a && b && a.value !== b.value)
  for (const [cookies, origin] of [[aCookies, runtime], [bCookies, runtimeB]]) {
    assert(cookies.every(cookie => cookie.name !== '__Host-labweaver_session'))
    assert(cookies.every(cookie => cookie.httpOnly && cookie.secure && cookie.domain === new URL(origin).hostname))
  }
  assert.notEqual(new URL(runtime).hostname, new URL(runtimeB).hostname)
  for (const request of runtimeRequests) {
    assert(!(await request.allHeaders()).cookie?.includes('__Host-labweaver_session='))
  }
  const blocked = page.waitForEvent('requestfailed', { predicate: request => request.url() === `${portal}/api/v1/auth/csrf` })
  const stolen = await page.evaluate(async portal => {
    try { return await (await fetch(`${portal}/api/v1/auth/csrf`, { credentials: 'include' })).text() }
    catch { return null }
  }, portal)
  assert.equal(stolen, null, 'untrusted script must not read a portal CSRF token')
  assert((await blocked).failure())
  assert.equal(await page.evaluate(async () => (await fetch('/api/v1/auth/session')).status), 403)

  for (const [method, path] of [['GET', 'cross-read'], ['POST', 'cross-write']]) {
    const target = `${runtimeB}/connect/${grantB}/${path}`
    const attempted = page.waitForRequest(request => request.url() === target && request.method() === method)
    const readable = await page.evaluate(async ({ target, method }) => {
      try {
        const response = await fetch(target, { method, credentials: 'include', ...(method === 'POST' ? { body: 'attack' } : {}) })
        return await response.text()
      } catch { return null }
    }, { target, method })
    assert.equal(readable, null, `runtime A must not ${method} runtime B`)
    assert.equal(new URL((await attempted).url()).origin, runtimeB)
    // Rust inspects the real Access wire request for same-site metadata and the
    // hash of B's automatically attached Cookie (CORS hides response/extra headers here).
  }
  // Same-origin path substitution also fails even with A's valid capability.
  assert.equal(await page.evaluate(async id => (await fetch(`/connect/${id}/cross-grant`)).status, grantB), 403)
  await page.evaluate(async target => { try { await fetch(target, { mode: 'no-cors', credentials: 'include' }) } catch {} }, `${runtimeB}/connect/${grantB}/cross-no-cors`)
  const posted = await page.evaluate(async () => {
    const response = await fetch(location.href, { method: 'POST', body: 'runtime-browser-post' })
    return { status: response.status, text: await response.text() }
  })
  assert.deepEqual(posted, { status: 200, text: 'runtime-browser-post' })
  assert(requestTargets.every(target => !target.includes('binding=')), 'request/access-log targets must never contain the binding secret')
  console.log('PASS: two real same-site runtime hosts; A cannot read/write B even with B cookie attached; no BFF cookie; portal API denied; fragment/CSRF POST handoff; no binding in request logs; own runtime POST works')
} finally {
  await browser.close()
}
