import { expect, test } from '@playwright/test'

/**
 * Cross-tab BFF session switch through the real useAuth + apiClient UI path.
 *
 * Two pages share one browser context (shared cookies). Route mocks flip the
 * session actor + CSRF token — not live multi-user Keycloak.
 *
 * The harness mirrors App.vue's sessionEpoch remount contract and uses the
 * production apiClient singleton so CSRF rejection drives useAuth refresh.
 * The test never manually clears the form or calls acknowledgeBffSessionRefreshed.
 */

const HARNESS = '/e2e/sdk/csrf-session-ui-harness.html'

function sessionBody(actorId) {
  return {
    actor: {
      actorId,
      roles: ['teacher'],
      expiresAt: '2099-01-01T00:00:00.000Z',
    },
    authorizationRevision: 1,
    expiresAt: '2099-01-01T00:00:00.000Z',
    scopes: [{ kind: 'global' }],
  }
}

async function installSharedBffMocks(context, identity, writes) {
  await context.route('**/api/v1/auth/csrf', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        csrfToken: identity.csrfToken,
        expiresAt: '2099-01-01T00:00:00.000Z',
      }),
    })
  })

  await context.route('**/api/v1/auth/session', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(sessionBody(identity.actorId)),
    })
  })

  await context.route('**/e2e/mock-login-b', async (route) => {
    identity.actorId = 'actor-b'
    identity.csrfToken = 'csrf-b'
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      headers: {
        'Set-Cookie': 'labweaver_test_session=session-b; Path=/',
      },
      body: JSON.stringify({ ok: true, actorId: 'actor-b' }),
    })
  })

  await context.route('**/api/v1/resource-requests', async (route) => {
    const request = route.request()
    if (request.method() !== 'POST') {
      await route.fulfill({ status: 405, body: 'method not allowed' })
      return
    }
    const csrf = request.headers()['x-csrf-token']
    const body = request.postDataJSON()
    writes.push({ method: 'POST', csrf, body, url: request.url() })
    if (csrf !== identity.csrfToken) {
      await route.fulfill({
        status: 403,
        contentType: 'application/problem+json',
        body: JSON.stringify({
          type: 'about:blank',
          title: 'Forbidden',
          status: 403,
          detail: 'CSRF token is not valid for the current session.',
          instance: '/api/v1/resource-requests',
          diagnosticCode: 'LW_AUTH_CSRF_REJECTED',
          requestId: 'e2e-csrf-reject',
          retryable: false,
        }),
      })
      return
    }
    await route.fulfill({
      status: 201,
      contentType: 'application/json',
      body: JSON.stringify({ ok: true, actorId: identity.actorId }),
    })
  })
}

test('two-page UI: session switch remounts form and refreshes displayed identity', async ({ page, context }) => {
  /** @type {{ actorId: string, csrfToken: string }} */
  const identity = { actorId: 'actor-a', csrfToken: 'csrf-a' }
  /** @type {Array<{ method: string, csrf?: string, body?: any, url: string }>} */
  const writes = []

  await context.addCookies([
    { name: 'labweaver_test_session', value: 'session-a', url: 'http://127.0.0.1:4174' },
  ])
  await installSharedBffMocks(context, identity, writes)

  // Page 1: real harness (useAuth + apiClient + sessionEpoch remount).
  await page.goto(HARNESS)
  await expect(page.getByTestId('session-actor')).toHaveText('actor-a')

  // Warm CSRF cache as A with a successful write, then enter a stale form body.
  await page.getByTestId('note-input').fill('warm-as-a')
  await page.getByTestId('submit-note').click()
  await expect(page.getByTestId('submit-result')).toContainText('ok:')
  expect(writes).toHaveLength(1)
  expect(writes[0]).toMatchObject({ csrf: 'csrf-a', body: { note: 'warm-as-a' } })

  await page.getByTestId('note-input').fill('stale-body-as-a')
  await expect(page.getByTestId('note-input')).toHaveValue('stale-body-as-a')
  const epochBefore = await page.getByTestId('session-epoch').innerText()

  // Page 2 (same context / cookie jar): switch shared session to B via the UI control.
  const page2 = await context.newPage()
  await page2.goto(HARNESS)
  await expect(page2.getByTestId('session-actor')).toHaveText('actor-a')
  await page2.getByTestId('switch-to-b').click()
  await expect(page2.getByTestId('session-actor')).toHaveText('actor-b')
  expect(identity).toMatchObject({ actorId: 'actor-b', csrfToken: 'csrf-b' })

  // Page 1 still shows A + the filled form until the CSRF rejection drives useAuth.
  await expect(page.getByTestId('session-actor')).toHaveText('actor-a')
  await expect(page.getByTestId('note-input')).toHaveValue('stale-body-as-a')

  // Submit from the old page — do NOT clear the form or acknowledge in the test.
  await page.getByTestId('submit-note').click()

  // Production path: CSRF reject → useAuth refresh → sessionEpoch remount clears form,
  // displayed identity becomes B. Test never calls acknowledgeBffSessionRefreshed.
  await expect(page.getByTestId('session-actor')).toHaveText('actor-b', { timeout: 15_000 })
  await expect(page.getByTestId('note-input')).toHaveValue('')
  const epochAfter = await page.getByTestId('session-epoch').innerText()
  expect(Number(epochAfter)).toBeGreaterThan(Number(epochBefore))

  // Stale A body was attempted only with csrf-a; never landed under csrf-b.
  const staleWrites = writes.filter((w) => w.body?.note === 'stale-body-as-a')
  expect(staleWrites.length).toBeGreaterThanOrEqual(1)
  expect(staleWrites.every((w) => w.csrf === 'csrf-a')).toBe(true)
  expect(writes.some((w) => w.csrf === 'csrf-b' && w.body?.note === 'stale-body-as-a')).toBe(false)

  // After UI reset, a subsequent write as B is allowed (barrier acknowledged by useAuth).
  await page.getByTestId('note-input').fill('composed-as-b')
  await page.getByTestId('submit-note').click()
  await expect(page.getByTestId('submit-result')).toContainText('ok:')
  expect(writes.some((w) => w.csrf === 'csrf-b' && w.body?.note === 'composed-as-b')).toBe(true)

  await page2.close()
})
