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
  getActiveCourseLlmPolicy,
  createProblemPackageUpload,
  completeProblemPackageUpload,
  createAgentRun,
  getAgentRun,
  cancelAgentRun,
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

  it('completes teacher material upload and agent run lifecycle', async () => {
    window.localStorage.setItem('access_token', 'fixture-teacher')
    const sha = (ch: string) => ch.repeat(64)

    const policy = await getActiveCourseLlmPolicy({ client: generatedClient, path: { courseId: 'course-101' } })
    expect(policy.error).toBeUndefined()
    expect(policy.data.courseId).toBe('course-101')

    const upload = await createProblemPackageUpload({
      client: generatedClient,
      path: { courseId: 'course-101' },
      headers: { 'Idempotency-Key': 'fixture-upload-001' },
      body: {
        files: [{ path: 'problem/README.md', sizeBytes: 128, sha256: sha('1'), mediaType: 'text/markdown' }],
        retentionPolicyRevision: policy.data.revision,
      },
    })
    expect(upload.error).toBeUndefined()
    expect(upload.data.uploadTargets).toHaveLength(1)

    const completed = await completeProblemPackageUpload({
      client: generatedClient,
      path: { courseId: 'course-101', uploadId: upload.data.id },
      headers: { 'Idempotency-Key': 'fixture-upload-002', 'If-Match': `"rev-${upload.data.revision}"` },
      body: { manifestSha256: sha('2') },
    })
    expect(completed.error).toBeUndefined()
    expect(completed.data.files).toHaveLength(1)

    const run = await createAgentRun({
      client: generatedClient,
      path: { courseId: 'course-101' },
      headers: { 'Idempotency-Key': 'fixture-run-001' },
      body: {
        packageId: completed.data.id,
        packageRevision: completed.data.revision,
        packageSha256: completed.data.manifestSha256,
        policyId: policy.data.id,
        policyRevision: policy.data.revision,
        requestedRuntime: 'container',
      },
    })
    expect(run.error).toBeUndefined()
    expect(run.data.state).toBe('running')

    const first = await getAgentRun({ client: generatedClient, path: { courseId: 'course-101', runId: run.data.id } })
    expect(first.error).toBeUndefined()
    const second = await getAgentRun({ client: generatedClient, path: { courseId: 'course-101', runId: run.data.id } })
    expect(second.data.state).toBe('succeeded')
  })

  it('rejects agent run creation with a stale policy revision', async () => {
    window.localStorage.setItem('access_token', 'fixture-teacher')
    const sha = (ch: string) => ch.repeat(64)

    const policy = await getActiveCourseLlmPolicy({ client: generatedClient, path: { courseId: 'course-101' } })
    const upload = await createProblemPackageUpload({
      client: generatedClient,
      path: { courseId: 'course-101' },
      headers: { 'Idempotency-Key': 'fixture-upload-003' },
      body: {
        files: [{ path: 'problem/main.py', sizeBytes: 64, sha256: sha('3'), mediaType: 'text/x-python' }],
        retentionPolicyRevision: policy.data.revision,
      },
    })
    const completed = await completeProblemPackageUpload({
      client: generatedClient,
      path: { courseId: 'course-101', uploadId: upload.data.id },
      headers: { 'Idempotency-Key': 'fixture-upload-004', 'If-Match': `"rev-${upload.data.revision}"` },
      body: { manifestSha256: sha('4') },
    })

    const stale = await createAgentRun({
      client: generatedClient,
      path: { courseId: 'course-101' },
      headers: { 'Idempotency-Key': 'fixture-run-002' },
      body: {
        packageId: completed.data.id,
        packageRevision: completed.data.revision,
        packageSha256: completed.data.manifestSha256,
        policyId: policy.data.id,
        policyRevision: policy.data.revision + 99,
        requestedRuntime: 'container',
      },
    })
    expect(stale.error).toMatchObject({ status: 409, diagnosticCode: 'REVISION_MISMATCH' })
  })

  it('cancels a running agent run with strong ETag', async () => {
    window.localStorage.setItem('access_token', 'fixture-teacher')
    const sha = (ch: string) => ch.repeat(64)

    const policy = await getActiveCourseLlmPolicy({ client: generatedClient, path: { courseId: 'course-101' } })
    const upload = await createProblemPackageUpload({
      client: generatedClient,
      path: { courseId: 'course-101' },
      headers: { 'Idempotency-Key': 'fixture-upload-005' },
      body: {
        files: [{ path: 'problem/lab.md', sizeBytes: 32, sha256: sha('5'), mediaType: 'text/markdown' }],
        retentionPolicyRevision: policy.data.revision,
      },
    })
    const completed = await completeProblemPackageUpload({
      client: generatedClient,
      path: { courseId: 'course-101', uploadId: upload.data.id },
      headers: { 'Idempotency-Key': 'fixture-upload-006', 'If-Match': `"rev-${upload.data.revision}"` },
      body: { manifestSha256: sha('6') },
    })
    const run = await createAgentRun({
      client: generatedClient,
      path: { courseId: 'course-101' },
      headers: { 'Idempotency-Key': 'fixture-run-003' },
      body: {
        packageId: completed.data.id,
        packageRevision: completed.data.revision,
        packageSha256: completed.data.manifestSha256,
        policyId: policy.data.id,
        policyRevision: policy.data.revision,
        requestedRuntime: 'virtual_machine',
      },
    })

    const cancelled = await cancelAgentRun({
      client: generatedClient,
      path: { courseId: 'course-101', runId: run.data.id },
      headers: { 'Idempotency-Key': 'fixture-run-004', 'If-Match': `"rev-${run.data.revision}"` },
    })
    expect(cancelled.error).toBeUndefined()

    const polled = await getAgentRun({ client: generatedClient, path: { courseId: 'course-101', runId: run.data.id } })
    expect(polled.data.state).toBe('cancelled')
  })
})
