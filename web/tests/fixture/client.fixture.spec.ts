import { describe, it, expect, beforeEach } from 'vitest'
import '@/api/client'
import { client as generatedClient } from '@/generated/contracts/client.gen'
import { listSshPublicKeys, createSshPublicKey } from '@/generated/contracts'
import { DATA_MODE } from '@/config/dataMode'

const describeFixture = DATA_MODE === 'fixture' ? describe : describe.skip

describeFixture('fixture adapter', () => {
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
})
