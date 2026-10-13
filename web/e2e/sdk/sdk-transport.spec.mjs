import { expect, test } from '@playwright/test'

const courseId = '01900000-0000-7000-8000-000000000001'
const projectId = '01900000-0000-7000-8000-000000000010'
const environmentId = '01900000-0000-7000-8000-000000000002'
const operationId = '01900000-0000-7000-8000-000000000003'
const resourceRequestId = '01900000-0000-7000-8000-000000000004'
const largeStreamSequence = '9007199254740993'

test.beforeEach(async ({ page }) => {
  await page.goto('/e2e/sdk/harness.html')
})

test('generated client performs authenticated GET and POST without duplicating /api/v1', async ({ page }) => {
  const requests = []
  await page.route('**/api/v1/environments**', async (route) => {
    const request = route.request()
    requests.push({
      method: request.method(),
      url: request.url(),
      authorization: request.headers().authorization,
      body: request.postDataJSON(),
    })
    if (request.method() === 'POST') {
      await route.fulfill({
        status: 202,
        contentType: 'application/json',
        body: JSON.stringify({
          operationId,
          environmentId,
          revision: 1,
          statusUrl: `/api/v1/environments/${environmentId}/operations/${operationId}`,
        }),
      })
      return
    }
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        items: [],
        nextCursor: null,
        snapshotSequence: largeStreamSequence,
        snapshotAt: '2026-07-15T12:00:00.000Z',
      }),
    })
  })

  const result = await page.evaluate(async ({ courseId, projectId, resourceRequestId }) => {
    const api = await import('/src/api/client.ts')
    const sdk = await import('/src/generated/contracts/index.ts')
    const client = api.createLabWeaverApiClient({
      baseUrl: window.location.origin,
      authentication: { mode: 'bearer', accessToken: async () => 'browser-conformance-token' },
    })
    const listed = await sdk.listEnvironments({ client, query: { courseId, limit: 25 }, throwOnError: true })
    const created = await sdk.createEnvironment({
      client,
      headers: { 'Idempotency-Key': 'sdk-conformance-create-1' },
      body: {
        courseId,
        projectId,
        resourceRequestId,
        releaseId: courseId,
        releaseVersion: 1,
        displayLabel: 'Browser contract',
      },
      throwOnError: true,
    })
    return { snapshotSequence: listed.data.snapshotSequence, environmentId: created.data.environmentId }
  }, { courseId, projectId, resourceRequestId })

  expect(result).toEqual({ snapshotSequence: largeStreamSequence, environmentId })
  expect(requests).toHaveLength(2)
  for (const request of requests) {
    expect(request.authorization).toBe('Bearer browser-conformance-token')
    expect(new URL(request.url).pathname).toBe('/api/v1/environments')
  }
  expect(requests[0].url).toContain(`courseId=${courseId}`)
  expect(requests[1].body).toMatchObject({
    displayLabel: 'Browser contract',
    projectId,
    resourceRequestId,
    courseId,
    releaseId: courseId,
    releaseVersion: 1,
  })
})

test('strictly decodes RFC 9457 and preserves stable diagnostics', async ({ page }) => {
  await page.route(`**/api/v1/environments/${environmentId}/operations/${operationId}`, async (route) => {
    await route.fulfill({
      status: 403,
      contentType: 'application/problem+json',
      body: JSON.stringify({
        type: '/problems/access-denied',
        title: 'Access denied',
        status: 403,
        detail: 'The actor cannot read this operation.',
        instance: '/requests/sdk-problem',
        diagnosticCode: 'LW_ACCESS_DENIED',
        requestId: 'sdk-problem',
        retryable: false,
      }),
    })
  })

  const error = await page.evaluate(async ({ environmentId, operationId }) => {
    const api = await import('/src/api/client.ts')
    const sdk = await import('/src/generated/contracts/index.ts')
    const client = api.createLabWeaverApiClient({
      baseUrl: window.location.origin,
      authentication: { mode: 'bearer', accessToken: async () => 'browser-conformance-token' },
    })
    try {
      await sdk.getEnvironmentOperation({ client, path: { environmentId, operationId }, throwOnError: true })
      return null
    } catch (caught) {
      const problem = api.decodeProblemDetails(caught)
      return {
        diagnosticCode: problem?.diagnosticCode,
        status: problem?.status,
        requestId: problem?.requestId,
      }
    }
  }, { environmentId, operationId })

  expect(error).toEqual({ diagnosticCode: 'LW_ACCESS_DENIED', status: 403, requestId: 'sdk-problem' })
})

test('BFF mode sends session credentials and synchronizer token only on mutations', async ({ page, context }) => {
  await context.addCookies([
    { name: 'labweaver_test_session', value: 'opaque-session', url: 'http://127.0.0.1:4178' },
  ])
  let csrfRequests = 0
  const requests = []
  await page.route('**/api/v1/auth/csrf', async (route) => {
    csrfRequests += 1
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ csrfToken: 'synchronizer-token', expiresAt: '2099-01-01T00:00:00.000Z' }),
    })
  })
  await page.route('**/api/v1/environments**', async (route) => {
    const request = route.request()
    requests.push({ method: request.method(), headers: request.headers(), body: request.postDataJSON() })
    if (request.method() === 'POST') {
      await route.fulfill({
        status: 202,
        contentType: 'application/json',
        body: JSON.stringify({ operationId, environmentId, revision: 1, statusUrl: '/operation' }),
      })
    } else {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ items: [], nextCursor: null, snapshotSequence: '1', snapshotAt: '2026-07-15T12:00:00.000Z' }),
      })
    }
  })

  await page.evaluate(async ({ courseId, projectId, resourceRequestId }) => {
    const api = await import('/src/api/client.ts')
    const sdk = await import('/src/generated/contracts/index.ts')
    const client = api.createLabWeaverApiClient({
      baseUrl: window.location.origin,
      authentication: { mode: 'bff' },
    })
    await sdk.listEnvironments({ client, query: { courseId }, throwOnError: true })
    await sdk.createEnvironment({
      client,
      headers: { 'Idempotency-Key': 'sdk-bff-create-1' },
      body: { courseId, projectId, resourceRequestId, releaseId: courseId, releaseVersion: 1 },
      throwOnError: true,
    })
  }, { courseId, projectId, resourceRequestId })

  expect(csrfRequests).toBe(1)
  expect(requests).toHaveLength(2)
  expect(requests[0].headers.cookie).toContain('labweaver_test_session=opaque-session')
  expect(requests[0].headers.authorization).toBeUndefined()
  expect(requests[0].headers['x-csrf-token']).toBeUndefined()
  expect(requests[1].headers.cookie).toContain('labweaver_test_session=opaque-session')
  expect(requests[1].headers.authorization).toBeUndefined()
  expect(requests[1].headers['x-csrf-token']).toBe('synchronizer-token')
  expect(requests[1].body).toMatchObject({ projectId, resourceRequestId, courseId, releaseId: courseId, releaseVersion: 1 })
})

test('preserves a stream cursor above Number.MAX_SAFE_INTEGER in SSE resume and events', async ({ page }) => {
  const requests = []
  await page.route(`**/api/v1/projects/${projectId}/events**`, async (route) => {
    const request = route.request()
    requests.push({
      url: request.url(),
      lastEventId: request.headers()['last-event-id'],
      authorization: request.headers().authorization,
    })
    await route.fulfill({
      status: 200,
      contentType: 'text/event-stream',
      body: `id: ${largeStreamSequence}\nevent: environment_changed\ndata: {"streamSequence":"${largeStreamSequence}"}\n\n`,
    })
  })

  const result = await page.evaluate(async ({ projectId, largeStreamSequence }) => {
    const api = await import('/src/api/client.ts')
    const sdk = await import('/src/generated/contracts/index.ts')
    const client = api.createLabWeaverApiClient({
      baseUrl: window.location.origin,
      authentication: { mode: 'bearer', accessToken: async () => 'browser-conformance-token' },
    })
    const events = await sdk.streamProjectEvents({
      client,
      path: { projectId },
      headers: { 'Last-Event-ID': largeStreamSequence },
      query: { after: largeStreamSequence },
    })
    const first = await events.stream.next()
    return first.value.streamSequence
  }, { projectId, largeStreamSequence })

  expect(result).toBe(largeStreamSequence)
  expect(requests).toHaveLength(1)
  expect(requests[0].authorization).toBe('Bearer browser-conformance-token')
  expect(requests[0].lastEventId).toBe(largeStreamSequence)
  expect(new URL(requests[0].url).pathname).toBe(`/api/v1/projects/${projectId}/events`)
  expect(new URL(requests[0].url).searchParams.get('after')).toBe(largeStreamSequence)
})

test('fails closed without a bearer and distinguishes cancellation', async ({ page }) => {
  let networkRequests = 0
  const pendingRoutes = []
  await page.route('**/api/v1/environments**', async (route) => {
    networkRequests += 1
    await new Promise((resolve) => pendingRoutes.push({ route, resolve }))
  })

  const authCode = await page.evaluate(async ({ courseId }) => {
    const api = await import('/src/api/client.ts')
    const sdk = await import('/src/generated/contracts/index.ts')
    const unauthenticated = api.createLabWeaverApiClient({
      baseUrl: window.location.origin,
      authentication: { mode: 'bearer', accessToken: async () => undefined },
    })
    let authCode
    try {
      await sdk.listEnvironments({ client: unauthenticated, query: { courseId }, throwOnError: true })
    } catch (error) {
      authCode = error.diagnosticCode
    }
    const authenticated = api.createLabWeaverApiClient({
      baseUrl: window.location.origin,
      authentication: { mode: 'bearer', accessToken: async () => 'browser-conformance-token' },
    })
    const controller = new AbortController()
    const request = sdk.listEnvironments({
      client: authenticated,
      query: { courseId },
      signal: controller.signal,
      throwOnError: true,
    })
    window.__sdkCancellation = {
      abort: () => controller.abort(),
      result: request.then(() => undefined, (error) => error.diagnosticCode),
    }
    return authCode
  }, { courseId })

  await expect.poll(() => networkRequests).toBe(1)
  await page.evaluate(() => window.__sdkCancellation.abort())
  const cancelCode = await page.evaluate(() => window.__sdkCancellation.result)

  const timeoutCode = await page.evaluate(async ({ courseId }) => {
    const api = await import('/src/api/client.ts')
    const sdk = await import('/src/generated/contracts/index.ts')
    const shortTimeout = api.createLabWeaverApiClient({
      baseUrl: window.location.origin,
      authentication: { mode: 'bearer', accessToken: async () => 'browser-conformance-token' },
      timeoutMilliseconds: 25,
    })
    let timeoutCode
    try {
      await sdk.listEnvironments({ client: shortTimeout, query: { courseId }, throwOnError: true })
    } catch (error) {
      timeoutCode = error.diagnosticCode
    }
    return timeoutCode
  }, { courseId })

  for (const pending of pendingRoutes) {
    await pending.route.abort().catch(() => undefined)
    pending.resolve()
  }

  expect({ authCode, cancelCode, timeoutCode }).toEqual({
      authCode: 'LW_SDK_AUTH_TOKEN_UNAVAILABLE',
      cancelCode: 'LW_SDK_REQUEST_CANCELLED',
      timeoutCode: 'LW_SDK_REQUEST_TIMEOUT',
    })
  expect(networkRequests).toBe(2)
})

test('SSE rejects missing bearer before any network request', async ({ page }) => {
  let networkRequests = 0
  await page.route(`**/api/v1/projects/${projectId}/events**`, async (route) => {
    networkRequests += 1
    await route.fulfill({
      status: 401,
      contentType: 'application/problem+json',
      body: JSON.stringify({
        type: '/problems/no-auth',
        title: 'No auth',
        status: 401,
        detail: 'NO_AUTH_ERROR',
        instance: '/requests/sse-no-auth',
        diagnosticCode: 'NO_AUTH_ERROR',
        requestId: 'sse-no-auth',
        retryable: false,
      }),
    })
  })

  const result = await page.evaluate(async ({ projectId }) => {
    const api = await import('/src/api/client.ts')
    const sdk = await import('/src/generated/contracts/index.ts')
    const client = api.createLabWeaverApiClient({
      baseUrl: window.location.origin,
      authentication: { mode: 'bearer', accessToken: async () => undefined },
    })
    try {
      const events = await sdk.streamProjectEvents({
        client,
        path: { projectId },
        query: { after: '0' },
      })
      // If construction incorrectly succeeds, advancing must still not be required
      // for the failure — but count any accidental fetch via the route counter.
      await events.stream.next()
      return { diagnosticCode: null }
    } catch (error) {
      return {
        diagnosticCode: error?.diagnosticCode ?? null,
        name: error?.name ?? null,
        message: typeof error?.message === 'string' ? error.message : null,
      }
    }
  }, { projectId })

  expect(result.diagnosticCode).toBe('LW_SDK_AUTH_TOKEN_UNAVAILABLE')
  expect(result.name).toBe('LabWeaverApiError')
  expect(networkRequests).toBe(0)
})


/**
 * Cross-tab BFF session switch regression (mocked BFF).
 *
 * CI limitation: full multi-user Keycloak login is not exercised here. Two pages
 * share one browser context (shared cookies) while route mocks flip the session
 * actor + CSRF token — the strongest feasible proof that tab A cannot silently
 * POST a preserved form body as user B after CSRF rejection.
 */
test('cross-tab session switch: CSRF reject blocks stale form submit until session refresh', async ({ page, context }) => {
  await context.addCookies([
    { name: 'labweaver_test_session', value: 'session-a', url: 'http://127.0.0.1:4174' },
  ])

  /** @type {{ actorId: string, csrfToken: string }} */
  const identity = { actorId: 'actor-a', csrfToken: 'csrf-a' }
  /** @type {Array<{ method: string, csrf?: string, body?: unknown, url: string }>} */
  const writes = []
  let csrfFetches = 0

  await page.route('**/api/v1/auth/csrf', async (route) => {
    csrfFetches += 1
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        csrfToken: identity.csrfToken,
        expiresAt: '2099-01-01T00:00:00.000Z',
      }),
    })
  })

  await page.route('**/api/v1/auth/session', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        actor: {
          actorId: identity.actorId,
          roles: ['teacher'],
          expiresAt: '2099-01-01T00:00:00.000Z',
        },
        authorizationRevision: 1,
        expiresAt: '2099-01-01T00:00:00.000Z',
        scopes: [{ kind: 'global' }],
      }),
    })
  })

  await page.route('**/api/v1/resource-requests', async (route) => {
    const request = route.request()
    if (request.method() !== 'POST') {
      await route.fulfill({ status: 405, body: 'method not allowed' })
      return
    }
    const csrf = request.headers()['x-csrf-token']
    const body = request.postDataJSON()
    writes.push({ method: 'POST', csrf, body, url: request.url() })
    // Reject when the synchronizer token does not match the current session.
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

  // Tab A: cache CSRF for actor A and preserve an in-DOM form body.
  await page.evaluate(async () => {
    const api = await import('/src/api/client.ts')
    const barrier = await import('/src/api/bffSessionBarrier.ts')
    const client = api.createLabWeaverApiClient({
      baseUrl: window.location.origin,
      authentication: { mode: 'bff' },
    })
    window.__csrfRegression = {
      client,
      api,
      barrier,
      formBody: { note: 'composed-as-user-a', amount: 42 },
      events: [],
      unsubscribe: barrier.subscribeBffCsrfSessionInvalidation(() => {
        window.__csrfRegression.events.push('session-invalidated')
      }),
    }
    await client.post({
      url: '/api/v1/resource-requests',
      body: { probe: 'warm-cache-as-a' },
      throwOnError: true,
    })
  })

  expect(csrfFetches).toBe(1)
  expect(writes).toHaveLength(1)
  expect(writes[0].csrf).toBe('csrf-a')

  // Tab B (same shared cookie jar): switch authenticated identity.
  identity.actorId = 'actor-b'
  identity.csrfToken = 'csrf-b'
  await context.addCookies([
    { name: 'labweaver_test_session', value: 'session-b', url: 'http://127.0.0.1:4174' },
  ])

  // Tab A still holds the old form; first submit uses stale csrf-a → rejected.
  const firstRetry = await page.evaluate(async () => {
    const { client, formBody, events } = window.__csrfRegression
    try {
      await client.post({
        url: '/api/v1/resource-requests',
        body: formBody,
        throwOnError: true,
      })
      return { ok: true, events: [...events] }
    } catch (error) {
      return {
        ok: false,
        status: error?.response?.status ?? null,
        diagnosticCode: error?.response?.data?.diagnosticCode ?? error?.diagnosticCode ?? null,
        events: [...events],
        writeBlocked: window.__csrfRegression.barrier.isBffWriteBlocked(),
      }
    }
  })

  expect(firstRetry).toMatchObject({
    ok: false,
    status: 403,
    diagnosticCode: 'LW_AUTH_CSRF_REJECTED',
    writeBlocked: true,
  })
  expect(firstRetry.events).toContain('session-invalidated')
  expect(writes).toHaveLength(2)
  expect(writes[1]).toMatchObject({
    csrf: 'csrf-a',
    body: { note: 'composed-as-user-a', amount: 42 },
  })

  // Blind second click must NOT fetch B's CSRF and POST A's form as B.
  const blockedRetry = await page.evaluate(async () => {
    const { client, formBody, api } = window.__csrfRegression
    try {
      await client.post({
        url: '/api/v1/resource-requests',
        body: formBody,
        throwOnError: true,
      })
      return { ok: true }
    } catch (error) {
      return {
        ok: false,
        diagnosticCode: error?.diagnosticCode ?? null,
        name: error?.name ?? null,
        writeBlocked: window.__csrfRegression.barrier.isBffWriteBlocked(),
      }
    }
  })

  expect(blockedRetry).toEqual({
    ok: false,
    diagnosticCode: 'LW_SDK_SESSION_REFRESH_REQUIRED',
    name: 'LabWeaverApiError',
    writeBlocked: true,
  })
  expect(writes).toHaveLength(2)
  expect(csrfFetches).toBe(1)

  // Simulate useAuth session refresh: reload identity, clear preserved form, acknowledge.
  const afterRefresh = await page.evaluate(async () => {
    const state = window.__csrfRegression
    const session = await (await fetch('/api/v1/auth/session', { credentials: 'include' })).json()
    // Pending form cleared as App.vue sessionEpoch remount would do.
    state.formBody = null
    state.barrier.acknowledgeBffSessionRefreshed()
    const blocked = state.barrier.isBffWriteBlocked()
    // A fresh form composed as the refreshed actor may proceed.
    await state.client.post({
      url: '/api/v1/resource-requests',
      body: { note: 'composed-as-user-b', actorId: session.actor.actorId },
      throwOnError: true,
    })
    return { sessionActor: session.actor.actorId, blockedBeforeWrite: blocked }
  })

  expect(afterRefresh).toEqual({ sessionActor: 'actor-b', blockedBeforeWrite: false })
  expect(csrfFetches).toBe(2)
  expect(writes).toHaveLength(3)
  expect(writes[2]).toMatchObject({
    csrf: 'csrf-b',
    body: { note: 'composed-as-user-b', actorId: 'actor-b' },
  })
  // Prove the old A form body never landed under B's token.
  expect(writes.some((w) => w.csrf === 'csrf-b' && w.body?.note === 'composed-as-user-a')).toBe(false)
})
