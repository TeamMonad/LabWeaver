import { shallowReactive } from 'vue'
import type { RouteLocationNormalized, Router } from 'vue-router'

/**
 * The navigation each router is still resolving, until it commits, fails or
 * is replaced by a newer one. Reactive, so code that defers work while a
 * navigation is pending re-runs once it finishes; shallow, so a stored
 * location keeps the identity the router passes to its hooks.
 */
const pending = shallowReactive(new WeakMap<Router, RouteLocationNormalized>())

function finish(router: Router, to: RouteLocationNormalized) {
  if (pending.get(router) === to) pending.delete(router)
}

/**
 * Track the navigation a router is resolving. Guards run in registration
 * order, so install this before any guard that waits (the session check):
 * otherwise a navigation only becomes visible after that wait.
 *
 * A navigation finishes with its own location: committed, aborted or
 * cancelled ones in `afterEach`, a throwing guard in `onError`. Only the
 * latest one clears, so an older navigation finishing never hides a newer
 * one. A duplicate navigation runs no guard and its location is never stored.
 * A redirect starts a new navigation that replaces the stored location.
 */
export function trackPendingNavigation(router: Router): void {
  router.beforeEach((to) => {
    pending.set(router, to)
  })
  router.afterEach((to) => finish(router, to))
  router.onError((_error, to) => finish(router, to))
}

/** Whether a navigation started on this router has not finished yet. */
export function navigationPending(router: Router): boolean {
  return pending.has(router)
}
