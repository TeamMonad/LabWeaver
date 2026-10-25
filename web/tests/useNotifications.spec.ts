import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { effectScope, nextTick, ref, type EffectScope } from 'vue'
import { LabWeaverApiError } from '@/api/client'
import { advanceSessionEpoch } from '@/api/session'
import { getNotificationUnreadCount, listNotifications, markNotificationRead, type Notification } from '@/generated/contracts'
import {
  MARK_READ_TIMEOUT_MS,
  NOTIFICATION_PAGE_SIZE,
  UNREAD_POLL_INTERVAL_MS,
  useNotificationList,
  useUnreadNotificationCount,
} from '@/composables/useNotifications'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getNotificationUnreadCount: vi.fn(),
    listNotifications: vi.fn(),
    markNotificationRead: vi.fn(),
  }
})

function notification(id: string, extra: Partial<Notification> = {}): Notification {
  return {
    id,
    eventId: `event-${id}`,
    projectId: 'project-1',
    kind: 'resource_decision',
    objectUrl: `/researcher/resources?projectId=project-1&requestId=request-${id}`,
    observedAt: '2026-10-20T08:00:00.000Z',
    summary: '资源申请已批准。',
    emailState: 'disabled',
    emailAttempts: 0,
    ...extra,
  }
}

function page(items: Notification[], nextCursor: string | null) {
  return { data: { items, nextCursor }, error: undefined } as never
}

function problem(status: number, diagnosticCode: string, requestId: string) {
  return {
    data: undefined,
    error: { type: 'about:blank', title: 'error', status, detail: 'server detail', diagnosticCode, requestId, retryable: status >= 500 },
  } as never
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => { resolve = done })
  return { promise, resolve }
}

beforeEach(() => {
  vi.mocked(listNotifications).mockReset()
  vi.mocked(markNotificationRead).mockReset()
  vi.mocked(getNotificationUnreadCount).mockReset().mockResolvedValue({ data: { unreadCount: 0 }, error: undefined } as never)
})

describe('useNotificationList paging', () => {
  it('follows nextCursor and stops requesting after the last page', async () => {
    vi.mocked(listNotifications)
      .mockResolvedValueOnce(page([notification('n1'), notification('n2')], 'cursor-1'))
      .mockResolvedValueOnce(page([notification('n2'), notification('n3')], null))
    const list = useNotificationList()

    await list.load()
    expect(listNotifications).toHaveBeenLastCalledWith({ query: { limit: NOTIFICATION_PAGE_SIZE } })
    expect(list.nextCursor).toBe('cursor-1')

    await list.loadMore()
    expect(listNotifications).toHaveBeenLastCalledWith({ query: { cursor: 'cursor-1', limit: NOTIFICATION_PAGE_SIZE } })
    // The overlapping row is shown once.
    expect(list.items.map((item) => item.id)).toEqual(['n1', 'n2', 'n3'])
    expect(list.nextCursor).toBeNull()

    await list.loadMore()
    expect(listNotifications).toHaveBeenCalledTimes(2)
  })

  it('keeps loaded pages when a later page fails and retries the same cursor', async () => {
    vi.mocked(listNotifications)
      .mockResolvedValueOnce(page([notification('n1')], 'cursor-1'))
      .mockResolvedValueOnce(problem(503, 'LW_NOTIFICATION_PERSISTENCE_FAILED', 'req-503'))
      .mockResolvedValueOnce(page([notification('n2')], null))
    const list = useNotificationList()
    await list.load()
    await list.loadMore()

    expect(list.items.map((item) => item.id)).toEqual(['n1'])
    expect(list.moreError?.requestId).toBe('req-503')

    await list.loadMore()
    expect(listNotifications).toHaveBeenLastCalledWith({ query: { cursor: 'cursor-1', limit: NOTIFICATION_PAGE_SIZE } })
    expect(list.items.map((item) => item.id)).toEqual(['n1', 'n2'])
    expect(list.moreError).toBeNull()
  })
})

describe('useNotificationList open', () => {
  it('marks one notification read only once, even for repeated clicks', async () => {
    vi.mocked(listNotifications).mockResolvedValueOnce(page([notification('n1')], null))
    const write = deferred<unknown>()
    vi.mocked(markNotificationRead).mockReturnValueOnce(write.promise as never)
    const list = useNotificationList()
    await list.load()

    const first = list.open(list.items[0]!)
    const second = await list.open(list.items[0]!)
    expect(second).toBeNull()
    write.resolve({ data: notification('n1', { readAt: '2026-10-20T08:05:00.000Z' }), error: undefined })
    expect(await first).toBe('/researcher/resources?projectId=project-1&requestId=request-n1')
    expect(markNotificationRead).toHaveBeenCalledTimes(1)
    expect(markNotificationRead).toHaveBeenCalledWith({ path: { notificationId: 'n1' }, signal: expect.any(AbortSignal) })
    expect(list.items[0]!.readAt).toBe('2026-10-20T08:05:00.000Z')
    expect(getNotificationUnreadCount).toHaveBeenCalled()

    // Already read: opening again navigates without another write.
    expect(await list.open(list.items[0]!)).toBe('/researcher/resources?projectId=project-1&requestId=request-n1')
    expect(markNotificationRead).toHaveBeenCalledTimes(1)
  })

  it('still opens the object when marking read fails for a transient reason', async () => {
    vi.mocked(listNotifications).mockResolvedValueOnce(page([notification('n1')], null))
    vi.mocked(markNotificationRead).mockResolvedValueOnce(problem(503, 'LW_NOTIFICATION_PERSISTENCE_FAILED', 'req-503'))
    const list = useNotificationList()
    await list.load()

    expect(await list.open(list.items[0]!)).toBe('/researcher/resources?projectId=project-1&requestId=request-n1')
    expect(list.items[0]!.readAt).toBeUndefined()
    expect(list.actionFailure).toBeNull()
    expect(list.opening).toBeNull()
  })

  it('does not open a notification the reader may no longer see after access is withdrawn', async () => {
    vi.mocked(listNotifications).mockResolvedValueOnce(page([notification('n1'), notification('n2')], null))
    vi.mocked(markNotificationRead).mockResolvedValueOnce(problem(404, 'LW_NOTIFICATION_NOT_FOUND', 'req-404'))
    const list = useNotificationList()
    await list.load()

    expect(await list.open(list.items[0]!)).toBeNull()
    expect(list.items.map((item) => item.id)).toEqual(['n2'])
    const diagnostic = list.actionFailure?.diagnostic
    expect(diagnostic?.message).toContain('这条通知已不可查看')
    expect(diagnostic?.nextStep).toContain('确认你的成员资格')
    expect(diagnostic?.code).toBe('LW_NOTIFICATION_NOT_FOUND')
    expect(diagnostic?.requestId).toBe('req-404')
    expect(diagnostic?.retryable).toBe(false)
    expect(getNotificationUnreadCount).toHaveBeenCalled()
  })

  it('refuses a link that leaves the console without marking it read', async () => {
    vi.mocked(listNotifications).mockResolvedValueOnce(page([notification('n1', { objectUrl: 'https://example.invalid/steal' })], null))
    const list = useNotificationList()
    await list.load()

    expect(await list.open(list.items[0]!)).toBeNull()
    expect(markNotificationRead).not.toHaveBeenCalled()
    expect(list.actionFailure?.diagnostic.code).toBe('NOTIFICATION_LINK_INVALID')
  })

  /** The SDK hands an error back as the result object itself, with `error` set. */
  const sessionDiscarded = () => Object.assign(
    new LabWeaverApiError('LW_SDK_SESSION_CHANGED', 'The signed-in session changed.'),
    { error: {} },
  )
  const answers = {
    success: () => ({ data: notification('n1', { readAt: '2026-10-20T08:05:00.000Z' }), error: undefined }),
    'withdrawn access': () => problem(404, 'LW_NOTIFICATION_NOT_FOUND', 'req-404'),
    'transient failure': () => problem(503, 'LW_NOTIFICATION_PERSISTENCE_FAILED', 'req-503'),
    'session discard': sessionDiscarded,
  }

  describe.each(Object.entries(answers))('a late %s answer', (_, answer) => {
    /**
     * Opens n1 and ends the open's validity with `invalidate` while its write
     * is pending. The write answers either after that (the open already
     * stopped) or just before it, in the same tick, so the answer is in hand
     * when the open resumes and has to be dropped by the validity check.
     */
    async function expectDropped(invalidate: (scope: EffectScope) => void, answerFirst: boolean) {
      vi.mocked(listNotifications).mockResolvedValueOnce(page([notification('n1'), notification('n2')], null))
      const write = deferred<unknown>()
      vi.mocked(markNotificationRead).mockReturnValueOnce(write.promise as never)
      const scope = effectScope()
      const list = scope.run(() => useNotificationList())!
      await list.load()
      const opened = list.open(list.items[0]!)
      const signal = vi.mocked(markNotificationRead).mock.calls[0]![0].signal!
      vi.mocked(getNotificationUnreadCount).mockClear()

      if (answerFirst) write.resolve(answer())
      invalidate(scope)
      expect(signal.aborted).toBe(true)
      expect(await opened).toBeNull()
      if (!answerFirst) write.resolve(answer())
      await new Promise((resolve) => setTimeout(resolve, 0))
      // Nothing of the answer is published, refreshed or navigated to.
      expect(list.items.map((item) => item.id)).toEqual(['n1', 'n2'])
      expect(list.items[0]!.readAt).toBeUndefined()
      expect(list.actionFailure).toBeNull()
      expect(list.opening).toBeNull()
      expect(getNotificationUnreadCount).not.toHaveBeenCalled()
      scope.stop()
    }

    it.each([false, true])('is dropped once the page that opened the notification is gone (answered first: %s)', async (answerFirst) => {
      await expectDropped((scope) => scope.stop(), answerFirst)
    })

    it.each([false, true])('is dropped once the session epoch the write started in ends (answered first: %s)', async (answerFirst) => {
      await expectDropped(() => advanceSessionEpoch(), answerFirst)
    })
  })

  it('does not treat a write the transport discarded for another actor as a transient failure', async () => {
    vi.mocked(listNotifications).mockResolvedValueOnce(page([notification('n1')], null))
    vi.mocked(markNotificationRead).mockResolvedValueOnce(sessionDiscarded() as never)
    const list = useNotificationList()
    await list.load()

    expect(await list.open(list.items[0]!)).toBeNull()
    expect(list.items[0]!.readAt).toBeUndefined()
    expect(list.actionFailure?.diagnostic.code).toBe('LW_SDK_SESSION_CHANGED')
    expect(list.actionFailure?.diagnostic.message).toBe('登录账号已变化，这次操作属于之前的账号，没有执行或结果已丢弃。')
  })

  it('opens the object after the deadline and cancels the write still in flight', async () => {
    vi.useFakeTimers()
    try {
      vi.mocked(listNotifications).mockResolvedValueOnce(page([notification('n1')], null))
      vi.mocked(markNotificationRead).mockReturnValueOnce(new Promise(() => {}) as never)
      const list = useNotificationList()
      await list.load()

      const opened = list.open(list.items[0]!)
      await vi.advanceTimersByTimeAsync(MARK_READ_TIMEOUT_MS - 1)
      expect(list.opening).toBe('n1')
      await vi.advanceTimersByTimeAsync(1)
      expect(await opened).toBe('/researcher/resources?projectId=project-1&requestId=request-n1')
      expect(vi.mocked(markNotificationRead).mock.calls[0]![0].signal!.aborted).toBe(true)
      expect(list.items[0]!.readAt).toBeUndefined()
      expect(list.opening).toBeNull()
    } finally {
      vi.useRealTimers()
    }
  })
})

describe('useUnreadNotificationCount', () => {
  let visibility: DocumentVisibilityState = 'visible'

  beforeEach(() => {
    vi.useFakeTimers()
    visibility = 'visible'
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => visibility })
  })

  afterEach(() => {
    vi.useRealTimers()
    Reflect.deleteProperty(document, 'visibilityState')
  })

  it('pauses polling while the tab is hidden and reads once on return', async () => {
    vi.mocked(getNotificationUnreadCount).mockResolvedValue({ data: { unreadCount: 2 }, error: undefined } as never)
    const scope = effectScope()
    const unread = scope.run(() => useUnreadNotificationCount(ref(true)))!
    await vi.advanceTimersByTimeAsync(0)
    expect(unread.value).toEqual({ kind: 'success', data: 2 })
    expect(getNotificationUnreadCount).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(UNREAD_POLL_INTERVAL_MS)
    expect(getNotificationUnreadCount).toHaveBeenCalledTimes(2)

    visibility = 'hidden'
    document.dispatchEvent(new Event('visibilitychange'))
    await vi.advanceTimersByTimeAsync(UNREAD_POLL_INTERVAL_MS * 3)
    expect(getNotificationUnreadCount).toHaveBeenCalledTimes(2)

    visibility = 'visible'
    document.dispatchEvent(new Event('visibilitychange'))
    await vi.advanceTimersByTimeAsync(0)
    expect(getNotificationUnreadCount).toHaveBeenCalledTimes(3)
    scope.stop()
  })

  it('reads nothing while signed out and clears the count on sign-out', async () => {
    vi.mocked(getNotificationUnreadCount).mockResolvedValue({ data: { unreadCount: 4 }, error: undefined } as never)
    const signedIn = ref(false)
    const scope = effectScope()
    const unread = scope.run(() => useUnreadNotificationCount(signedIn))!
    await vi.advanceTimersByTimeAsync(UNREAD_POLL_INTERVAL_MS)
    expect(getNotificationUnreadCount).not.toHaveBeenCalled()

    signedIn.value = true
    await nextTick()
    await vi.advanceTimersByTimeAsync(0)
    expect(unread.value).toEqual({ kind: 'success', data: 4 })

    signedIn.value = false
    await nextTick()
    expect(unread.value).toEqual({ kind: 'idle' })
    scope.stop()
  })
})
