import { computed, getCurrentScope, onScopeDispose, ref, watch, type Ref } from 'vue'
import type { AccessGrantSchema, EnvironmentEndpointSchema, EnvironmentInstanceSchema } from '@/generated/contracts'
import type { AsyncState } from '@/types/async'
import { connectionReadiness, type ConnectionReadinessKind } from '@/utils/environmentConnection'

const RECHECK_INTERVAL_MS = 5000
/** Automatic re-checks stop after this long without progress; the user can re-check by hand. */
const RECHECK_WINDOW_MS = 10 * 60_000
const EXPIRY_SKEW_MS = 1000
/** setTimeout stores its delay in a signed 32-bit integer. */
const MAX_TIMER_DELAY_MS = 2_147_483_647

/** Readiness states that can change on the server without any user action. */
const WAITING_KINDS: Partial<Record<ConnectionReadinessKind, true>> = {
  'unknown': true,
  'no-endpoints': true,
  'endpoints-pending': true,
  'endpoints-unhealthy': true,
  'needs-regrant': true,
}

export interface WatchedAccess {
  endpoints: AsyncState<EnvironmentEndpointSchema[]>
  grant: AsyncState<AccessGrantSchema>
  creating: boolean
  loadEndpoints: (silent?: boolean) => Promise<unknown>
  loadCurrentGrant: (silent?: boolean) => Promise<unknown>
}

/**
 * Keeps the connection readiness of one open environment current while the
 * page is visible: endpoints and the current access grant are re-read every
 * few seconds while the server may still change them, the grant is re-read
 * when it expires, and both are re-read when the page becomes visible again.
 * Re-reads are silent so the last answer stays on screen, and every timer is
 * cancelled when the page hides or the view goes away.
 */
export function useConnectionWatch(
  instance: Ref<Pick<EnvironmentInstanceSchema, 'desiredState' | 'observedState'> | null>,
  access: WatchedAccess,
) {
  const now = ref(Date.now())
  const autoChecking = ref(false)
  let recheckTimer: ReturnType<typeof setTimeout> | undefined
  let expiryTimer: ReturnType<typeof setTimeout> | undefined
  let windowStartedAt: number | null = null

  const readinessKind = computed(() => instance.value
    ? connectionReadiness(instance.value, access.endpoints, access.grant, '', now.value).kind
    : null)

  const pageVisible = () => typeof document === 'undefined' || document.visibilityState !== 'hidden'

  async function refresh() {
    now.value = Date.now()
    // An access grant that is being issued is followed by its own activation reads.
    await Promise.all([
      access.loadEndpoints(true),
      access.creating ? Promise.resolve() : access.loadCurrentGrant(true),
    ])
    now.value = Date.now()
  }

  function scheduleRecheck() {
    clearTimeout(recheckTimer)
    const kind = readinessKind.value
    if (!kind || !WAITING_KINDS[kind]) {
      windowStartedAt = null
      autoChecking.value = false
      return
    }
    if (!pageVisible()) {
      autoChecking.value = false
      return
    }
    windowStartedAt ??= Date.now()
    if (Date.now() - windowStartedAt >= RECHECK_WINDOW_MS) {
      autoChecking.value = false
      return
    }
    autoChecking.value = true
    recheckTimer = setTimeout(async () => {
      await refresh()
      scheduleRecheck()
    }, RECHECK_INTERVAL_MS)
  }

  /** User-requested re-check; it also restarts the automatic window. */
  async function recheck() {
    windowStartedAt = null
    clearTimeout(recheckTimer)
    await refresh()
    scheduleRecheck()
  }

  watch(readinessKind, scheduleRecheck, { immediate: true })

  watch(
    () => access.grant.kind === 'success' && access.grant.data.state === 'active' ? access.grant.data.expiresAt : null,
    (expiresAt) => {
      clearTimeout(expiryTimer)
      if (!expiresAt) return
      const delay = Math.min(Math.max(Date.parse(expiresAt) - Date.now() + EXPIRY_SKEW_MS, 0), MAX_TIMER_DELAY_MS)
      expiryTimer = setTimeout(() => {
        void refresh().then(scheduleRecheck)
      }, delay)
    },
    { immediate: true },
  )

  function onVisibilityChange() {
    if (!pageVisible()) {
      clearTimeout(recheckTimer)
      autoChecking.value = false
      return
    }
    if (instance.value) void recheck()
  }
  if (typeof document !== 'undefined') document.addEventListener('visibilitychange', onVisibilityChange)

  if (getCurrentScope()) {
    onScopeDispose(() => {
      clearTimeout(recheckTimer)
      clearTimeout(expiryTimer)
      if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', onVisibilityChange)
    })
  }

  return { now, autoChecking, recheck }
}
