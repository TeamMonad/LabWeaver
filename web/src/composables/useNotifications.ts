import { reactive, ref, watch, type Ref } from 'vue'
import {
  getNotificationUnreadCount,
  listNotifications,
  markNotificationRead,
  type Notification,
} from '@/generated/contracts'
import { sessionEpoch } from '@/api/session'
import { useVisiblePolling } from '@/composables/useVisiblePolling'
import { diagnosticFromError, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { notificationObjectLocation } from '@/utils/notifications'

/** Unread count cadence while the tab is visible; a hidden tab does not poll. */
export const UNREAD_POLL_INTERVAL_MS = 60_000
export const NOTIFICATION_PAGE_SIZE = 20
/** A slow mark-read write must not hold the reader on the list; marking is idempotent. */
export const MARK_READ_TIMEOUT_MS = 5_000

// One unread count per tab: the top bar polls it and the list page refreshes
// it after a read. It is always a server answer, never a local decrement, so
// it cannot drift from what the list shows.
const unread = ref<AsyncState<number>>({ kind: 'idle' })
let unreadGeneration = 0

function clearUnread() {
  unreadGeneration += 1
  unread.value = { kind: 'idle' }
}

/** Read the unread count again now. */
export async function refreshUnreadCount(): Promise<void> {
  const generation = ++unreadGeneration
  if (unread.value.kind !== 'success') unread.value = { kind: 'loading' }
  const result = await getNotificationUnreadCount()
  if (generation !== unreadGeneration) return
  unread.value = result.error
    ? { kind: 'error', diagnostic: diagnosticFromError(result.error, 'NOTIFICATION_UNREAD_COUNT_FAILED', '暂时无法读取未读通知数。') }
    : { kind: 'success', data: result.data.unreadCount }
}

/**
 * Keep the unread count current while `enabled` (an account is signed in).
 * Installed once by the top bar. Signing out, or another account taking over
 * the session, clears the previous count before anything new is shown.
 */
export function useUnreadNotificationCount(enabled: Ref<boolean>) {
  useVisiblePolling(refreshUnreadCount, enabled, UNREAD_POLL_INTERVAL_MS, UNREAD_POLL_INTERVAL_MS)
  watch(enabled, (value) => {
    clearUnread()
    if (value) void refreshUnreadCount()
  }, { immediate: true })
  watch(sessionEpoch, () => {
    clearUnread()
    if (enabled.value) void refreshUnreadCount()
  })
  return unread
}

export interface NotificationActionFailure {
  notificationId: string
  diagnostic: DiagnosticViewModel
}

/**
 * The reader's notifications, newest first, one cursor page at a time.
 * Opening a notification marks it read on the server first; the object page
 * then authorizes the reader on its own, so a link never grants access.
 */
export function useNotificationList() {
  const items = ref<Notification[]>([])
  const state = ref<AsyncState<Notification[]>>({ kind: 'idle' })
  const nextCursor = ref<string | null>(null)
  const loadingMore = ref(false)
  const moreError = ref<DiagnosticViewModel | null>(null)
  const actionFailure = ref<NotificationActionFailure | null>(null)
  const opening = ref<string | null>(null)
  let generation = 0

  function publish() {
    state.value = items.value.length > 0 ? { kind: 'success', data: items.value } : { kind: 'empty' }
  }

  async function load() {
    const current = ++generation
    loadingMore.value = false
    moreError.value = null
    actionFailure.value = null
    state.value = { kind: 'loading', message: '正在读取通知…' }
    const result = await listNotifications({ query: { limit: NOTIFICATION_PAGE_SIZE } })
    if (current !== generation) return
    if (result.error) {
      items.value = []
      nextCursor.value = null
      state.value = { kind: 'error', diagnostic: diagnosticFromError(result.error, 'NOTIFICATION_LIST_FAILED', '暂时无法读取通知。') }
      return
    }
    items.value = result.data.items
    nextCursor.value = result.data.nextCursor ?? null
    publish()
  }

  async function loadMore() {
    const cursor = nextCursor.value
    if (!cursor || loadingMore.value) return
    const current = generation
    loadingMore.value = true
    moreError.value = null
    const result = await listNotifications({ query: { cursor, limit: NOTIFICATION_PAGE_SIZE } })
    if (current !== generation) return
    loadingMore.value = false
    if (result.error) {
      // Loaded pages stay; retrying asks for the same cursor again.
      moreError.value = diagnosticFromError(result.error, 'NOTIFICATION_LIST_FAILED', '暂时无法读取更早的通知，已显示的通知不受影响。')
      return
    }
    const known = new Set(items.value.map((item) => item.id))
    items.value = [...items.value, ...result.data.items.filter((item) => !known.has(item.id))]
    nextCursor.value = result.data.nextCursor ?? null
    publish()
  }

  /**
   * Mark the notification read, then return where to go; null keeps the
   * reader here with the reason in `actionFailure`. Only Control's answer that
   * the reader can no longer see the notification stops the link. Any other
   * mark-read failure still opens the object: the notification simply stays
   * unread. An already read notification opens without another write, and a
   * click while a write is in flight sends nothing.
   */
  async function open(notification: Notification): Promise<string | null> {
    if (opening.value) return null
    const location = notificationObjectLocation(notification.objectUrl)
    if (!location) {
      actionFailure.value = {
        notificationId: notification.id,
        diagnostic: makeDiagnostic('NOTIFICATION_LINK_INVALID', '这条通知的对象链接无效，页面不会打开它。', false),
      }
      return null
    }
    actionFailure.value = null
    if (notification.readAt) return location
    opening.value = notification.id
    const current = generation
    try {
      const result = await markNotificationRead({ path: { notificationId: notification.id }, timeout: MARK_READ_TIMEOUT_MS })
      if (current !== generation) return null
      if (result.error) {
        const diagnostic = diagnosticFromError(result.error, 'NOTIFICATION_MARK_READ_FAILED', '暂时无法把这条通知标记为已读。')
        if (diagnostic.status !== 404) return location
        // Control rechecks recipient and membership on every read: 404 means
        // the reader may no longer see it, so neither the row nor the link stays.
        items.value = items.value.filter((item) => item.id !== notification.id)
        publish()
        actionFailure.value = {
          notificationId: notification.id,
          diagnostic: makeDiagnostic('LW_NOTIFICATION_NOT_FOUND', '', false, {
            requestId: diagnostic.requestId,
            status: diagnostic.status,
            traceId: diagnostic.traceId,
          }),
        }
        void refreshUnreadCount()
        return null
      }
      const updated = result.data
      items.value = items.value.map((item) => (item.id === updated.id ? updated : item))
      publish()
      void refreshUnreadCount()
      return location
    } finally {
      opening.value = null
    }
  }

  return reactive({ items, state, nextCursor, loadingMore, moreError, actionFailure, opening, load, loadMore, open })
}
