import { notFound, problem, unauthorized } from '../diagnostics'
import { createSshKeyFixtures } from '../scenarios/sshKeys'
import { nowIso } from '../utils/clock'
import { nextId, nextRevision, resetSequence } from '../utils/sequence'
import type { FixtureHandler, FixtureRequest } from '../types'
import type { SshPublicKeySchema } from '@/generated/contracts'

const fixtures = createSshKeyFixtures()

function bearerActorId(req: FixtureRequest): string | null {
  const auth = req.headers.Authorization ?? req.headers.authorization ?? ''
  const match = /^Bearer fixture-(?<role>[a-z-]+)$/.exec(auth)
  return match?.groups?.role ? `fixture-actor-${match.groups.role}` : null
}

export const listSshPublicKeys: FixtureHandler = (req) => {
  const actorId = bearerActorId(req)
  if (!actorId) return unauthorized()
  return { status: 200, data: { items: fixtures.list } }
}

export const createSshPublicKey: FixtureHandler = (req) => {
  const actorId = bearerActorId(req)
  if (!actorId) return unauthorized()

  const idempotency = req.headers['Idempotency-Key'] ?? req.headers['idempotency-key']
  if (!idempotency) {
    return problem(400, 'IDEMPOTENCY_KEY_MISSING', '缺少 Idempotency-Key header', false)
  }

  const body = req.body as { publicKeyOpenssh?: string } | undefined
  const publicKeyOpenssh = body?.publicKeyOpenssh?.trim() ?? ''
  if (!publicKeyOpenssh.startsWith('ssh-')) {
    return problem(400, 'SSH_KEY_MALFORMED', '公钥格式不是有效的 OpenSSH 格式', false)
  }

  resetSequence()
  const created: SshPublicKeySchema = {
    id: nextId('key'),
    actorId,
    algorithm: publicKeyOpenssh.startsWith('ssh-ed25519') ? 'ed25519' : 'rsa',
    fingerprintSha256: 'c'.repeat(64),
    createdAt: nowIso(),
    revision: nextRevision(),
  }
  return { status: 201, data: created }
}

export const deleteSshPublicKey: FixtureHandler = (req) => {
  const actorId = bearerActorId(req)
  if (!actorId) return unauthorized()

  const ifMatch = req.headers['If-Match'] ?? req.headers['if-match']
  if (!ifMatch) {
    return problem(400, 'IF_MATCH_MISSING', '缺少 If-Match header', false)
  }

  const keyId = (req.url.split('/').pop() ?? '').trim()
  if (!fixtures.list.some((k) => k.id === keyId)) {
    return notFound(req.url)
  }

  return { status: 204, data: undefined }
}
