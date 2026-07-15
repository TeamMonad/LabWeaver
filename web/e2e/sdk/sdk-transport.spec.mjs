import { expect, test } from '@playwright/test'

const courseId = '01900000-0000-7000-8000-000000000001'
const environmentId = '01900000-0000-7000-8000-000000000002'
const operationId = '01900000-0000-7000-8000-000000000003'
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

  const result = await page.evaluate(async ({ courseId }) => {
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
      body: { courseId, releaseId: courseId, releaseVersion: 1, displayLabel: 'Browser contract' },
      throwOnError: true,
    })
    return { snapshotSequence: listed.data.snapshotSequence, environmentId: created.data.environmentId }
  }, { courseId })

  expect(result).toEqual({ snapshotSequence: largeStreamSequence, environmentId })
  expect(requests).toHaveLength(2)
  for (const request of requests) {
    expect(request.authorization).toBe('Bearer browser-conformance-token')
    expect(new URL(request.url).pathname).toBe('/api/v1/environments')
  }
  expect(requests[0].url).toContain(`courseId=${courseId}`)
  expect(requests[1].body.displayLabel).toBe('Browser contract')
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
        diagnosticCode: caught.diagnosticCode,
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
    requests.push({ method: request.method(), headers: request.headers() })
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

  await page.evaluate(async ({ courseId }) => {
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
      body: { courseId, releaseId: courseId, releaseVersion: 1 },
      throwOnError: true,
    })
  }, { courseId })

  expect(csrfRequests).toBe(1)
  expect(requests).toHaveLength(2)
  expect(requests[0].headers.cookie).toContain('labweaver_test_session=opaque-session')
  expect(requests[0].headers.authorization).toBeUndefined()
  expect(requests[0].headers['x-csrf-token']).toBeUndefined()
  expect(requests[1].headers.cookie).toContain('labweaver_test_session=opaque-session')
  expect(requests[1].headers.authorization).toBeUndefined()
  expect(requests[1].headers['x-csrf-token']).toBe('synchronizer-token')
})

test('preserves a stream cursor above Number.MAX_SAFE_INTEGER in SSE resume and events', async ({ page }) => {
  const requests = []
  await page.route('**/api/v1/events**', async (route) => {
    const request = route.request()
    requests.push({ url: request.url(), lastEventId: request.headers()['last-event-id'] })
    await route.fulfill({
      status: 200,
      contentType: 'text/event-stream',
      body: `id: ${largeStreamSequence}\nevent: environment_changed\ndata: {"streamSequence":"${largeStreamSequence}"}\n\n`,
    })
  })

  const result = await page.evaluate(async ({ courseId, largeStreamSequence }) => {
    const api = await import('/src/api/client.ts')
    const sdk = await import('/src/generated/contracts/index.ts')
    const client = api.createLabWeaverApiClient({
      baseUrl: window.location.origin,
      authentication: { mode: 'bearer', accessToken: async () => 'browser-conformance-token' },
    })
    const events = await sdk.streamCourseEvents({
      client,
      headers: { 'Last-Event-ID': largeStreamSequence },
      query: { courseId, after: largeStreamSequence },
    })
    const first = await events.stream.next()
    return first.value.streamSequence
  }, { courseId, largeStreamSequence })

  expect(result).toBe(largeStreamSequence)
  expect(requests).toHaveLength(1)
  expect(requests[0].lastEventId).toBe(largeStreamSequence)
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
