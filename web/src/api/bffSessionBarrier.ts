/**
 * Minimal BFF cross-tab session-change barrier shared by the API client and useAuth.
 *
 * HttpOnly session cookies are invisible to JS, so a CSRF rejection is the signal that
 * another tab replaced the shared cookie. Callers must refresh authenticated UI / clear
 * pending form state before writes resume — never auto-replay the failed write.
 */

type BffCsrfSessionListener = () => void

const listeners = new Set<BffCsrfSessionListener>()
let writeBlocked = false

/** Subscribe to CSRF-driven session invalidation (e.g. useAuth reloads the actor). */
export function subscribeBffCsrfSessionInvalidation(listener: BffCsrfSessionListener): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

/** Close the write gate and notify listeners. Idempotent while already blocked. */
export function notifyBffCsrfSessionInvalidation(): void {
  writeBlocked = true
  for (const listener of [...listeners]) {
    try {
      listener()
    } catch {
      // Listener failures must not mask the original CSRF rejection.
    }
  }
}

export function isBffWriteBlocked(): boolean {
  return writeBlocked
}

/** Lift the write gate after identity / UI has been refreshed. */
export function acknowledgeBffSessionRefreshed(): void {
  writeBlocked = false
}

/** Reset module state between unit tests. */
export function resetBffSessionBarrierForTests(): void {
  writeBlocked = false
  listeners.clear()
}
