import type { SshPublicKeySchema } from '@/generated/contracts'
import { notFound, problem, unauthorized, conflict, preconditionFailed } from '../diagnostics'
import type { FixtureHandler } from '../types'
import { nowIso } from '../utils/clock'
import { nextId, nextRevision } from '../utils/sequence'
import { parseActor } from '../stores/actorStore'

const ALLOWED_ALGORITHMS = [
  'ssh-rsa',
  'ssh-ed25519',
  'ecdsa-sha2-nistp256',
  'ecdsa-sha2-nistp384',
  'ecdsa-sha2-nistp521',
  'sk-ssh-ed25519@openssh.com',
  'sk-ecdsa-sha2-nistp256@openssh.com',
] as const

type AllowedAlgorithm = (typeof ALLOWED_ALGORITHMS)[number]

interface StoredKey {
  key: SshPublicKeySchema
  bodyHash: string
}

const keys: SshPublicKeySchema[] = []
const idempotencyMap = new Map<string, StoredKey>()

function seedKeys(): void {
  keys.length = 0
  idempotencyMap.clear()

  keys.push(
    {
      id: nextId('key'),
      actorId: 'fixture-actor-student',
      algorithm: 'ed25519',
      fingerprintSha256: 'SHA256:' + 'a'.repeat(43),
      createdAt: nowIso(),
      revision: nextRevision(),
    },
    {
      id: nextId('key'),
      actorId: 'fixture-actor-student',
      algorithm: 'rsa',
      fingerprintSha256: 'SHA256:' + 'b'.repeat(43),
      createdAt: nowIso(),
      revision: nextRevision(),
      rsaBits: 4096,
    },
  )
}

seedKeys()

export function resetSshKeyStore(): void {
  seedKeys()
}

function isAllowedAlgorithm(value: string): value is AllowedAlgorithm {
  return (ALLOWED_ALGORITHMS as readonly string[]).includes(value)
}

function normalizeOpenSshKey(raw: string): { algorithm: AllowedAlgorithm; base64: string } | null {
  const trimmed = raw.trim()
  const parts = trimmed.split(' ')
  if (parts.length < 2) return null
  const algorithm = parts[0]
  const base64 = parts[1]
  if (!isAllowedAlgorithm(algorithm)) return null
  if (!/^[A-Za-z0-9+/=]+$/.test(base64)) return null
  return { algorithm, base64 }
}

function base64Decode(base64: string): Uint8Array | null {
  try {
    const binary = atob(base64)
    return Uint8Array.from(binary, (char) => char.charCodeAt(0))
  } catch {
    return null
  }
}

async function sha256Fingerprint(base64: string): Promise<string | null> {
  const bytes = base64Decode(base64)
  if (!bytes) return null

  let digest: ArrayBuffer
  if (typeof process !== 'undefined' && process.versions?.node) {
    const nodeCrypto = await import('crypto')
    digest = nodeCrypto.createHash('sha256').update(bytes).digest()
  } else {
    digest = await crypto.subtle.digest('SHA-256', bytes)
  }

  const array = Array.from(new Uint8Array(digest))
  const base64Hash = btoa(String.fromCharCode(...array))
    .replace(/=/g, '')
    .replace(/\+/g, '+')
    .replace(/\//g, '/')
  return `SHA256:${base64Hash}`
}

function bodyHash(body: { publicKeyOpenssh: string }): string {
  return body.publicKeyOpenssh.trim()
}

function algorithmToSchema(algorithm: AllowedAlgorithm): SshPublicKeySchema['algorithm'] {
  switch (algorithm) {
    case 'ssh-rsa':
      return 'rsa'
    case 'ssh-ed25519':
    case 'sk-ssh-ed25519@openssh.com':
      return 'ed25519'
    case 'ecdsa-sha2-nistp256':
    case 'sk-ecdsa-sha2-nistp256@openssh.com':
      return 'ecdsa'
    case 'ecdsa-sha2-nistp384':
      return 'ecdsa'
    case 'ecdsa-sha2-nistp521':
      return 'ecdsa'
    default:
      return 'rsa'
  }
}

export const listSshPublicKeys: FixtureHandler = (req) => {
  const actor = parseActor(req)
  if (!actor) return unauthorized()
  return { status: 200, data: { items: keys.filter((k) => k.actorId === actor.actorId) } }
}

export const createSshPublicKey: FixtureHandler = async (req) => {
  const actor = parseActor(req)
  if (!actor) return unauthorized()

  const idempotency = req.headers['Idempotency-Key'] ?? req.headers['idempotency-key']
  if (!idempotency) {
    return problem(400, 'IDEMPOTENCY_KEY_MISSING', '缺少 Idempotency-Key header', false)
  }

  const body = req.body as { publicKeyOpenssh?: string } | undefined
  const publicKeyOpenssh = body?.publicKeyOpenssh?.trim() ?? ''
  const normalized = normalizeOpenSshKey(publicKeyOpenssh)
  if (!normalized) {
    return problem(400, 'SSH_KEY_MALFORMED', '公钥格式不是有效的 OpenSSH 格式', false)
  }

  const fingerprint = await sha256Fingerprint(normalized.base64)
  if (!fingerprint) {
    return problem(400, 'SSH_KEY_MALFORMED', '公钥 base64 解码失败', false)
  }

  const cached = idempotencyMap.get(idempotency)
  if (cached) {
    if (cached.bodyHash !== bodyHash({ publicKeyOpenssh })) {
      return conflict('Idempotency-Key 已被用于不同的请求')
    }
    return { status: 200, data: cached.key }
  }

  const created: SshPublicKeySchema = {
    id: nextId('key'),
    actorId: actor.actorId,
    algorithm: algorithmToSchema(normalized.algorithm),
    fingerprintSha256: fingerprint,
    createdAt: nowIso(),
    revision: nextRevision(),
  }
  if (created.algorithm === 'rsa') {
    created.rsaBits = 4096
  }

  keys.push(created)
  idempotencyMap.set(idempotency, { key: created, bodyHash: bodyHash({ publicKeyOpenssh }) })

  return { status: 201, data: created }
}

export const deleteSshPublicKey: FixtureHandler = (req) => {
  const actor = parseActor(req)
  if (!actor) return unauthorized()

  const ifMatch = req.headers['If-Match'] ?? req.headers['if-match']
  if (!ifMatch) {
    return problem(400, 'IF_MATCH_MISSING', '缺少 If-Match header', false)
  }

  const keyId = (req.url.split('/').pop() ?? '').trim()
  const index = keys.findIndex((k) => k.id === keyId)
  if (index < 0) {
    return notFound(req.url)
  }

  const key = keys[index]
  if (key.actorId !== actor.actorId) {
    return forbidden('只能删除自己的 SSH 公钥')
  }
  if (String(key.revision) !== ifMatch) {
    return preconditionFailed('If-Match revision 不匹配')
  }

  keys.splice(index, 1)
  return { status: 204, data: undefined }
}
