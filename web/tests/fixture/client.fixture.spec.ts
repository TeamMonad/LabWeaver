import { describe, it, expect, beforeAll, beforeEach } from 'vitest'
import { initializeSdkClient } from '@/api/client'
import { client as generatedClient } from '@/generated/contracts/client.gen'
import {
  listSshPublicKeys,
  createSshPublicKey,
  getEnvironment,
  listEnvironmentEndpoints,
  createAccessGrant,
  revokeAccessGrant,
} from '@/generated/contracts'
import { DATA_MODE } from '@/config/dataMode'

const describeFixture = DATA_MODE === 'fixture' ? describe : describe.skip

describeFixture('fixture adapter', () => {
  beforeAll(async () => {
    await initializeSdkClient()
  })

  beforeEach(() => {
    window.localStorage.clear()
  })

  it('returns deterministic SSH keys for fixture-student', async () => {
    window.localStorage.setItem('access_token', 'fixture-student')
    const result = await listSshPublicKeys({ client: generatedClient })

    expect(result.error).toBeUndefined()
    expect(result.data.items).toHaveLength(2)
    expect(result.data.items[0].algorithm).toBe('ed25519')
    expect(result.data.items[1].algorithm).toBe('rsa')
  })

  it('creates a new SSH key when Idempotency-Key is present', async () => {
    window.localStorage.setItem('access_token', 'fixture-student')
    const result = await createSshPublicKey({
      client: generatedClient,
      headers: { 'Idempotency-Key': 'fixture-key-001' },
      body: {
        publicKeyOpenssh:
          'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDIhz2GK/XCUj4i6Q5yQJNL1MKDXETe1aM1lHYMGt2SQ user@example.com',
      },
    })

    expect(result.error).toBeUndefined()
    expect(result.data.algorithm).toBe('ed25519')
  })

  it('rejects unauthenticated requests with 401', async () => {
    const result = await listSshPublicKeys({ client: generatedClient })
    expect(result.error).toMatchObject({ status: 401, diagnosticCode: 'UNAUTHENTICATED' })
  })

  it('creates and revokes an access grant with strong ETag If-Match', async () => {
    window.localStorage.setItem('access_token', 'fixture-student')

    const env = await getEnvironment({ client: generatedClient, path: { environmentId: 'env-lifecycle-failure' } })
    expect(env.error).toBeUndefined()
    expect(env.data.courseId).toBe('course-101')

    const eps = await listEnvironmentEndpoints({ client: generatedClient, path: { environmentId: 'env-lifecycle-failure' } })
    expect(eps.error).toBeUndefined()
    expect(eps.data.items.length).toBeGreaterThan(0)

    const created = await createAccessGrant({
      client: generatedClient,
      path: { environmentId: 'env-lifecycle-failure' },
      headers: { 'Idempotency-Key': 'fixture-grant-revoke-001' },
      body: {
        courseId: env.data.courseId,
        environmentId: env.data.id,
        environmentRevision: env.data.revision,
        endpointIds: eps.data.items.map((ep) => ep.id),
        expiresAt: env.data.eligibilityExpiresAt,
      },
    })
    expect(created.error).toBeUndefined()
    expect(created.data.state).toBe('active')

    const revoked = await revokeAccessGrant({
      client: generatedClient,
      path: { grantId: created.data.id },
      headers: { 'Idempotency-Key': 'fixture-grant-revoke-002', 'If-Match': `"rev-${created.data.revision}"` },
      body: { grantId: created.data.id, reasonCode: 'user_revoked' },
    })
    expect(revoked.error).toBeUndefined()
    expect(revoked.data?.operationId).toBeDefined()
    expect(revoked.data?.statusUrl).toBe(`/api/v1/access-grants/${created.data.id}`)
  })
})
