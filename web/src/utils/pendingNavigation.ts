import type { RouteLocationNormalized, Router } from 'vue-router'

/** The navigation each router is still resolving, until it commits, fails or is replaced by a newer one. */
const pending = new WeakMap<Router, RouteLocationNormalized>()

/**
 * Track the navigation a router is resolving. Guards run in registration
 * order, so install this before any guard that waits (the session check):
 * otherwise a navigation only becomes visible after that wait.
 */
export function trackPendingNavigation(router: Router): void {
  router.beforeEach((to) => {
    pending.set(router, to)
  })
  // Cancelled navigations finish with their own location, so only the latest one clears.
  router.afterEach((to) => {
    if (pending.get(router) === to) pending.delete(router)
  })
  router.onError(() => {
    pending.delete(router)
  })
}

/** Whether a navigation started on this router has not finished yet. */
export function navigationPending(router: Router): boolean {
  return pending.has(router)
}
