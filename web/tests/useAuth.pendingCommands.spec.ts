import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
// Warm the module graph once (see useAuth.bff.spec.ts); each test re-imports after resetModules.
import '@/composables/useAuth'

const ACTOR_A = '01900000-0000-7000-8000-00000000000a'
const ACTOR_B = '01900000-0000-7000-8000-00000000000b'
const PENDING_A = `labweaver.pending-command:${ACTOR_A}:manual-work-template:project-1`
const PENDING_B = `labweaver.pending-command:${ACTOR_B}:manual-work-template:project-1`

function session(actorId: string) {
  return new Response(JSON.stringify({
    actor: { actorId, roles: ['teacher'], expiresAt: '2099-01-01T00:00:00.000Z' },
    authorizationRevision: 1,
    expiresAt: '2099-01-01T00:00:00.000Z',
    scopes: [{ kind: 'global' }],
  }), { status: 200, headers: { 'content-type': 'application/json' } })
}

function storeCommand(key: string) {
  window.sessionStorage.setItem(key, JSON.stringify({ fingerprint: 'f', idempotencyKey: `key-${key}`, data: {} }))
}

/** Loads useAuth in BFF mode with the given session answers, in order. */
async function authWith(...answers: Array<Response | Error>) {
  vi.stubEnv('VITE_API_AUTH_MODE', 'bff')
  vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => {
    const next = answers.shift()
    if (next instanceof Error) throw next
    return next
  }))
  // Dynamic on purpose: useAuth reads the auth mode and keeps session state at module load, so each test re-evaluates it.
  const { useAuth } = await import('@/composables/useAuth')
  return useAuth(vi.fn())
}

describe('pending commands across identity changes (real loadUser)', () => {
  beforeEach(() => {
    vi.resetModules()
    window.sessionStorage.clear()
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.unstubAllEnvs()
  })

  it.each([
    ['a 503 session read', new Response('{}', { status: 503 })],
    ['a network failure', new TypeError('Failed to fetch')],
  ])('keeps the actor\'s unconfirmed commands when %s leaves the identity unknown', async (_label, failure) => {
    const auth = await authWith(session(ACTOR_A), failure)
    await auth.loadUser()
    storeCommand(PENDING_A)

    await auth.loadUser()
    // Authentication still fails closed …
    expect(auth.isAuthenticated.value).toBe(false)
    expect(auth.error.value).not.toBeNull()
    // … but nothing proves the actor signed out, so their commands stay replayable.
    expect(window.sessionStorage.getItem(PENDING_A)).not.toBeNull()
  })

  it('keeps commands when the same actor is loaded again after a failure', async () => {
    const auth = await authWith(session(ACTOR_A), new Response('{}', { status: 503 }), session(ACTOR_A))
    await auth.loadUser()
    storeCommand(PENDING_A)
    await auth.loadUser()
    await auth.loadUser()
    expect(auth.isAuthenticated.value).toBe(true)
    expect(window.sessionStorage.getItem(PENDING_A)).not.toBeNull()
  })

  it('drops every pending command when the session read says the user is signed out', async () => {
    const auth = await authWith(session(ACTOR_A), new Response('{}', { status: 401 }))
    await auth.loadUser()
    storeCommand(PENDING_A)
    await auth.loadUser()
    expect(window.sessionStorage.getItem(PENDING_A)).toBeNull()
  })

  it('drops every pending command on an explicit sign-out', async () => {
    const auth = await authWith(
      session(ACTOR_A),
      new Response(JSON.stringify({ csrfToken: 'csrf' }), { status: 200 }),
      new Response(JSON.stringify({ logoutUrl: 'https://identity.example.test/logout' }), { status: 200 }),
    )
    await auth.loadUser()
    storeCommand(PENDING_A)
    await auth.logout()
    expect(window.sessionStorage.getItem(PENDING_A)).toBeNull()
  })

  it('drops the previous actor\'s commands when another actor is confirmed, even after an unknown identity', async () => {
    const auth = await authWith(session(ACTOR_A), new TypeError('Failed to fetch'), session(ACTOR_B))
    await auth.loadUser()
    storeCommand(PENDING_A)
    await auth.loadUser()
    expect(window.sessionStorage.getItem(PENDING_A)).not.toBeNull()
    storeCommand(PENDING_B)

    await auth.loadUser()
    expect(window.sessionStorage.getItem(PENDING_A)).toBeNull()
    expect(window.sessionStorage.getItem(PENDING_B)).not.toBeNull()
  })
})

describe('pending commands held in page memory across identity changes (real loadUser)', () => {
  const SCOPE = 'credits-admin:project-1:{"action":"grant"}'
  const command = { fingerprint: SCOPE, idempotencyKey: 'key-memory-a', data: { reason: 'A 的发放' } }

  beforeEach(() => {
    vi.resetModules()
    window.sessionStorage.clear()
    // Session storage refuses every write, so commands live only in page memory.
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new DOMException('full', 'QuotaExceededError') })
  })

  afterEach(() => {
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
    vi.unstubAllEnvs()
  })

  /** Same module instance as the freshly imported useAuth. */
  async function pendingCommands() {
    return import('@/utils/pendingCommands')
  }

  it.each([
    ['a 503 session read', new Response('{}', { status: 503 })],
    ['a network failure', new TypeError('Failed to fetch')],
  ])('keeps a memory-only command when %s leaves the identity unknown', async (_label, failure) => {
    const auth = await authWith(session(ACTOR_A), failure)
    const pending = await pendingCommands()
    await auth.loadUser()
    expect(pending.writePendingCommand(ACTOR_A, SCOPE, command)).toBe(false)

    await auth.loadUser()
    expect(pending.readPendingCommand(ACTOR_A, SCOPE)?.idempotencyKey).toBe('key-memory-a')
    expect(pending.listPendingCommands(ACTOR_A, 'credits-admin:project-1:')).toEqual([
      expect.objectContaining({ scope: SCOPE, persisted: false }),
    ])
  })

  it('drops a memory-only command on an explicit sign-out and does not restore it for the same actor later', async () => {
    const auth = await authWith(
      session(ACTOR_A),
      new Response(JSON.stringify({ csrfToken: 'csrf' }), { status: 200 }),
      new Response(JSON.stringify({ logoutUrl: 'https://identity.example.test/logout' }), { status: 200 }),
      session(ACTOR_A),
    )
    const pending = await pendingCommands()
    await auth.loadUser()
    pending.writePendingCommand(ACTOR_A, SCOPE, command)

    await auth.logout()
    await auth.loadUser()
    expect(auth.isAuthenticated.value).toBe(true)
    expect(pending.readPendingCommand(ACTOR_A, SCOPE)).toBeNull()
    expect(pending.listPendingCommands(ACTOR_A, 'credits-admin:')).toEqual([])
  })

  it('drops a memory-only command when the session read says the user is signed out', async () => {
    const auth = await authWith(session(ACTOR_A), new Response('{}', { status: 401 }))
    const pending = await pendingCommands()
    await auth.loadUser()
    pending.writePendingCommand(ACTOR_A, SCOPE, command)
    await auth.loadUser()
    expect(pending.readPendingCommand(ACTOR_A, SCOPE)).toBeNull()
  })

  it('drops the previous actor\'s memory-only command when another actor is confirmed, and switching back does not restore it', async () => {
    const auth = await authWith(session(ACTOR_A), session(ACTOR_B), session(ACTOR_A))
    const pending = await pendingCommands()
    await auth.loadUser()
    pending.writePendingCommand(ACTOR_A, SCOPE, command)

    await auth.loadUser()
    expect(pending.readPendingCommand(ACTOR_A, SCOPE)).toBeNull()
    await auth.loadUser()
    expect(pending.readPendingCommand(ACTOR_A, SCOPE)).toBeNull()
    expect(pending.listPendingCommands(ACTOR_A, 'credits-admin:')).toEqual([])
  })
})
