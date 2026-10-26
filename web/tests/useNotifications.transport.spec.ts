import type { InternalAxiosRequestConfig } from 'axios'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
// Warm the module graph once; each test re-imports after vi.resetModules() so
// the transport's CSRF token cache starts cold.
import '@/composables/useNotifications'

// The real BFF transport: the generated SDK, the auth interceptor and its CSRF
// prefetch. Only the HTTP adapter and the CSRF GET are replaced.
vi.mock('@/config', () => ({
  API_AUTH_MODE: 'bff',
  API_BASE_URL: '/',
}))

vi.mock('@/composables/useAuth', () => ({
  getOidcAccessToken: vi.fn(async () => undefined),
}))

const notification = {
  id: 'n1',
  eventId: 'event-n1',
  projectId: 'project-1',
  kind: 'resource_decision',
  objectUrl: '/researcher/resources?projectId=project-1&requestId=request-n1',
  observedAt: '2026-10-20T08:00:00.000Z',
  summary: '资源申请已批准。',
  emailState: 'disabled',
  emailAttempts: 0,
} as const

const OBJECT_PATH = '/researcher/resources?projectId=project-1&requestId=request-n1'

/** A fresh transport (cold CSRF cache) with its HTTP adapter replaced. */
async function freshTransport() {
  // Dynamic re-import after vi.resetModules() is the point: the CSRF token
  // cache lives in module state, so a static import would share a warm cache.
  vi.resetModules()
  const { default: axios } = await import('axios')
  const { initializeSdkClient } = await import('@/api/client')
  const { client } = await import('@/generated/contracts/client.gen')
  const notifications = await import('@/composables/useNotifications')
  await initializeSdkClient()
  // Writes go out only for a confirmed actor holding a token issued to it.
  const { recordSessionActor } = await import('@/api/session')
  recordSessionActor('actor-a')
  const adapter = vi.fn(async (config: InternalAxiosRequestConfig) => {
    const data = String(config.url).endsWith('/read')
      ? { ...notification, readAt: '2026-10-20T08:05:00.000Z' }
      : { items: [notification], nextCursor: null }
    return { data, status: 200, statusText: 'OK', headers: {}, config }
  })
  const transport = client.getConfig().axios
  if (!transport || 'Axios' in transport) throw new Error('SDK transport not initialized')
  transport.defaults.adapter = adapter
  const writes = () => adapter.mock.calls.filter(([config]) => String(config.url).endsWith('/read'))
  return { axios, adapter, writes, notifications }
}

describe('opening a notification over the BFF transport', () => {
  beforeEach(() => {
    vi.useFakeTimers()
  })

  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
  })

  it('counts a cold CSRF prefetch against the deadline and never sends the write after it', async () => {
    const { axios, writes, notifications } = await freshTransport()
    // Nothing cached: the write first asks the BFF for a token, which is slow.
    let issueToken!: (value: unknown) => void
    const csrfGet = vi.spyOn(axios, 'get').mockReturnValue(new Promise((resolve) => { issueToken = resolve }) as never)
    const list = notifications.useNotificationList()
    await list.load()

    const opened = list.open(list.items[0]!)
    await vi.advanceTimersByTimeAsync(0)
    expect(csrfGet).toHaveBeenCalledWith('/api/v1/auth/csrf', expect.anything())
    await vi.advanceTimersByTimeAsync(notifications.MARK_READ_TIMEOUT_MS)
    // The deadline covers the prefetch: the object opens and the row stays unread.
    expect(await opened).toBe(OBJECT_PATH)
    expect(list.items[0]!.readAt).toBeUndefined()
    expect(list.opening).toBeNull()

    // The token arrives late; the abandoned write is not dispatched with it.
    issueToken({ data: { csrfToken: 'csrf-late', actorId: 'actor-a', expiresAt: new Date(Date.now() + 600_000).toISOString() } })
    await vi.advanceTimersByTimeAsync(0)
    expect(writes()).toHaveLength(0)
  })

  it('sends the write with the token when the prefetch answers within the deadline', async () => {
    const { axios, writes, notifications } = await freshTransport()
    vi.spyOn(axios, 'get').mockResolvedValue({
      data: { csrfToken: 'csrf-fresh', actorId: 'actor-a', expiresAt: new Date(Date.now() + 600_000).toISOString() },
    } as never)
    const list = notifications.useNotificationList()
    await list.load()

    expect(await list.open(list.items[0]!)).toBe(OBJECT_PATH)
    expect(writes()).toHaveLength(1)
    const [write] = writes()[0]!
    expect(write.method).toBe('post')
    expect(write.headers['X-CSRF-Token']).toBe('csrf-fresh')
    expect(list.items[0]!.readAt).toBe('2026-10-20T08:05:00.000Z')
  })
})
