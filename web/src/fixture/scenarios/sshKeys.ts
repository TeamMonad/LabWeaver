import { nowIso } from '../utils/clock'
import { nextId, nextRevision, resetSequence } from '../utils/sequence'
import type { SshPublicKeySchema } from '@/generated/contracts'

export interface SshKeyFixtures {
  valid: SshPublicKeySchema
  empty: SshPublicKeySchema[]
  list: SshPublicKeySchema[]
}

export function createSshKeyFixtures(): SshKeyFixtures {
  resetSequence()
  return {
    empty: [],
    valid: {
      id: nextId('key'),
      actorId: 'fixture-actor-student',
      algorithm: 'ed25519',
      fingerprintSha256: 'a'.repeat(64),
      createdAt: nowIso(),
      revision: nextRevision(),
    },
    list: [
      {
        id: nextId('key'),
        actorId: 'fixture-actor-student',
        algorithm: 'ed25519',
        fingerprintSha256: 'a'.repeat(64),
        createdAt: nowIso(),
        revision: nextRevision(),
      },
      {
        id: nextId('key'),
        actorId: 'fixture-actor-student',
        algorithm: 'rsa',
        fingerprintSha256: 'b'.repeat(64),
        createdAt: nowIso(),
        revision: nextRevision(),
        rsaBits: 4096,
      },
    ],
  }
}
