// Generated SDK integration owned by #203 backend. No mocked business routes or UI state.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { chromium } from '@playwright/test'
import { createServer } from 'vite'

const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const ciphertext = Buffer.from(fixture.ciphertext, 'base64')
const digest = createHash('sha256').update(ciphertext).digest('hex')
process.env.VITE_API_PROXY_TARGET = fixture.controlUrl
const server = await createServer({
  configFile: 'vite.config.ts',
  logLevel: 'silent',
  server: { host: '127.0.0.1', port: 0, strictPort: false },
})
let browser
try {
  await server.listen()
  const address = server.httpServer.address()
  const origin = `http://127.0.0.1:${address.port}`
  browser = await chromium.launch({
    ...(process.env.LABWEAVER_TEST_CHROMIUM ? { executablePath: process.env.LABWEAVER_TEST_CHROMIUM } : {}),
  })
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } })
  const errors = []
  const warnings = []
  const rejectedResponses = []
  const uploads = []
  page.on('pageerror', (error) => errors.push(error.message))
  page.on('console', (message) => {
    if (['error', 'warning'].includes(message.type())) warnings.push(message.text())
  })
  page.on('response', (response) => {
    if (response.status() >= 400) rejectedResponses.push({
      path: new URL(response.url()).pathname, status: response.status(),
    })
  })
  page.on('request', (request) => {
    const path = new URL(request.url()).pathname
    assert(!path.startsWith('/api/v1/api/v1'))
    if (request.method() === 'POST' && path.endsWith('/bundle')) {
      uploads.push(request.headers()['content-type'])
    }
  })
  await page.goto(`${origin}/e2e/sdk/harness.html`)
  assert.equal(await page.title(), 'LabWeaver SDK transport conformance')
  assert.equal(await page.locator('main').textContent(), 'SDK transport conformance harness')
  assert.equal(await page.locator('vite-error-overlay').count(), 0)

  const result = await page.evaluate(async (input) => {
    const api = await import('/src/api/client.ts')
    const sdk = await import('/src/generated/contracts/index.ts')
    const client = api.createLabWeaverApiClient({ baseUrl: location.origin,
      authentication: { mode: 'bearer', accessToken: async () => input.gatewayToken } })
    const headers = (revision, key, actor = input.admin) => ({
      'x-labweaver-actor-id': actor, 'x-labweaver-session-id': input.session,
      'If-Match': `"rev-${revision}"`, 'Idempotency-Key': key,
    })
    const rejected = async (call, status, code) => {
      try { await call(); throw new Error('expected rejection') } catch (error) {
        const problem = api.decodeProblemDetails(error)
        if (problem?.status !== status || (code && problem?.diagnosticCode !== code)) {
          throw new Error(`unexpected rejection ${problem?.status}/${problem?.diagnosticCode}`)
        }
      }
    }
    const read = () => sdk.getPlatformConfiguration({ client, headers: headers(0, 'browser-read'), throwOnError: true })
    const initial = await read()
    if (initial.data.revision !== 0 || initial.headers.etag !== '"rev-0"') throw new Error('initial fence')
    await rejected(() => sdk.getPlatformConfiguration({ client,
      headers: headers(0, 'browser-denied', input.otherActor), throwOnError: true }), 403)
    const update = { expectedRevision: 0, configuration: initial.data.defaults }
    const saved = await sdk.updatePlatformConfiguration({ client, body: update,
      headers: headers(0, 'browser-settings'), throwOnError: true })
    const desired = saved.data.desired
    await rejected(() => sdk.updatePlatformConfiguration({ client, body: update,
      headers: headers(0, 'browser-stale-settings'), throwOnError: true }), 409,
      'LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT')
    await rejected(() => sdk.applyPlatformConfiguration({ client,
      body: { expectedRevision: saved.data.revision, desiredRevision: desired.revision },
      headers: headers(saved.data.revision, 'browser-unregistered'), throwOnError: true }), 409,
      'LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED')
    const file = new File([new Uint8Array(input.bytes)], 'synthetic.age', { type: 'application/octet-stream' })
    const registration = { client, path: { revision: desired.revision },
      query: { expectedRevision: saved.data.revision, configurationSha256: desired.configurationSha256 },
      body: file, headers: headers(saved.data.revision, 'browser-register'), throwOnError: true }
    const registered = await sdk.registerPlatformConfigurationBundle(registration)
    const replay = await sdk.registerPlatformConfigurationBundle(registration)
    if (JSON.stringify(replay.data) !== JSON.stringify(registered.data)) throw new Error('registration replay')
    await rejected(() => sdk.registerPlatformConfigurationBundle({ ...registration,
      headers: headers(saved.data.revision, 'browser-stale-register') }), 409,
      'LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT')
    const bound = await sdk.getPlatformConfigurationBundle({ client, path: { revision: desired.revision },
      headers: headers(0, 'browser-bundle-read'), throwOnError: true })
    const bundle = registered.data.registeredBundle
    if (bundle.encryptedBundleSha256 !== input.digest || bundle.encryptedBytes !== input.bytes.length ||
        JSON.stringify(bundle) !== JSON.stringify(bound.data) || registered.data.applied !== null ||
        registered.data.latestApply !== null) throw new Error('received ciphertext binding')
    const requested = await sdk.applyPlatformConfiguration({ client,
      body: { expectedRevision: registered.data.revision, desiredRevision: desired.revision },
      headers: headers(registered.data.revision, 'browser-apply'), throwOnError: true })
    if (requested.status !== 202 || requested.data.latestApply.stage !== 'requested' ||
        requested.data.applied !== null) throw new Error('request is not applied')
    const reread = await read()
    if (JSON.stringify(reread.data) !== JSON.stringify(requested.data)) throw new Error('durable read')
    return { apply: requested.data.latestApply, desiredRevision: desired.revision, registered: bundle }
  }, { ...fixture, executorToken: undefined, ciphertext: undefined, evidenceDirectory: undefined,
    bytes: [...ciphertext], digest })
  assert.equal(result.registered.encryptedBundleSha256, digest)
  assert.equal(uploads.length, 3)
  for (const contentType of uploads) assert.equal(contentType, 'application/octet-stream')

  // The executor reports a synthetic failure through the real internal endpoint.
  // No command exit code is converted into a successful deployment observation.
  let attempt = result.apply
  for (const stage of ['validating', 'applying', 'failed']) {
    const response = await fetch(`${fixture.controlUrl}/internal/v1/platform-configuration/applies/${attempt.id}`, {
      method: 'PUT', headers: { Authorization: `Bearer ${fixture.executorToken}`,
        'Content-Type': 'application/json', 'If-Match': `"rev-${attempt.revision}"`,
        'Idempotency-Key': `browser-executor-${stage}` },
      body: JSON.stringify({ expectedRevision: attempt.revision, stage,
        failureReason: stage === 'failed' ? 'execution_failed' : null,
        observedConfigurationSha256: null, observedEncryptedBundleSha256: null }),
    })
    assert.equal(response.status, 200, `executor ${stage} rejected`)
    attempt = (await response.json()).latestApply
  }
  const final = await page.evaluate(async ({ gatewayToken, admin, session, failedId, desiredRevision }) => {
    const api = await import('/src/api/client.ts')
    const sdk = await import('/src/generated/contracts/index.ts')
    const client = api.createLabWeaverApiClient({ baseUrl: location.origin,
      authentication: { mode: 'bearer', accessToken: async () => gatewayToken } })
    const headers = (revision, key) => ({ 'x-labweaver-actor-id': admin, 'x-labweaver-session-id': session,
      'If-Match': `"rev-${revision}"`, 'Idempotency-Key': key })
    const read = async () => (await sdk.getPlatformConfiguration({ client,
      headers: headers(0, 'browser-read-after-failure'), throwOnError: true })).data
    const failed = await read()
    if (failed.latestApply.stage !== 'failed' || failed.latestApply.failedStage !== 'applying' ||
        failed.latestApply.nextStep !== 'reconcile_external_result' || failed.appliedState !== 'unverified' ||
        failed.applied !== null) throw new Error('failure facts')
    const retry = await sdk.applyPlatformConfiguration({ client,
      body: { expectedRevision: failed.revision, desiredRevision },
      headers: headers(failed.revision, 'browser-retry'), throwOnError: true })
    if (retry.data.latestApply.previousApplyId !== failedId || retry.data.appliedState !== 'unverified') {
      throw new Error('retry replaced failure')
    }
    const configuration = structuredClone(retry.data.desired.configuration)
    configuration.lifecycle.preemptionGraceSeconds = 90
    const edited = await sdk.updatePlatformConfiguration({ client,
      body: { expectedRevision: retry.data.revision, configuration },
      headers: headers(retry.data.revision, 'browser-new-desired'), throwOnError: true })
    if (edited.data.registeredBundle !== null) throw new Error('new desired retained old binding')
    const history = await sdk.getPlatformConfigurationApply({ client, path: { applyId: failedId },
      headers: headers(0, 'browser-failed-history'), throwOnError: true })
    const oldBundle = await sdk.getPlatformConfigurationBundle({ client, path: { revision: desiredRevision },
      headers: headers(0, 'browser-old-bundle'), throwOnError: true })
    return { stage: history.data.stage, digest: oldBundle.data.encryptedBundleSha256,
      revision: (await read()).revision, nextStep: edited.data.latestApply.nextStep }
  }, { gatewayToken: fixture.gatewayToken, admin: fixture.admin, session: fixture.session,
    failedId: result.apply.id, desiredRevision: result.desiredRevision })
  assert.equal(final.stage, 'failed')
  assert.equal(final.digest, digest)
  assert.equal(final.nextStep, 'reconcile_external_result')
  assert.deepEqual(errors, [])
  // Chromium logs the intentionally rejected HTTP requests; all other warnings fail.
  assert(rejectedResponses.every(({ path, status }) =>
    ([403, 409].includes(status) && path.startsWith('/api/v1/admin/platform-configuration')) ||
    (status === 404 && path === '/favicon.ico')), 'unexpected rejected browser resource')
  assert(warnings.every(message => /^Failed to load resource: the server responded with a status of (403|404|409)\b/.test(message)),
    `unexpected console warning: ${warnings.slice(0, 5).join('; ')}`)
  assert.equal(await page.locator('vite-error-overlay').count(), 0)
  await page.screenshot({ path: join(fixture.evidenceDirectory, 'sdk-harness.png') })
  console.log(`PLATFORM_CONFIGURATION_BROWSER_OK revision=${final.revision} uploads=${uploads.length}`)
} catch (error) {
  // Assertion payloads can include request input. Only emit the bounded message.
  const message = error instanceof Error ? error.message.split('\n')[0] : 'browser integration failed'
  console.error(message.replace(/Bearer\s+\S+/g, 'Bearer [redacted]'))
  process.exitCode = 1
} finally {
  await browser?.close()
  await server.close()
}
