import { readonly, ref } from 'vue'

/**
 * Which browser session API calls belong to.
 *
 * The BFF session cookie is HttpOnly and shared by every tab, so another tab
 * can sign out or sign in as someone else without this one noticing. useAuth
 * advances the epoch when the signed-in actor changes or the session is
 * confirmed ended; the API transport discards the result of any request
 * started under an older epoch, and the AppShell re-creates the routed page so
 * no form or list of the previous actor survives.
 *
 * A CSRF or session rejection means the cookie may already belong to someone
 * else. Until useAuth has read the session again, requests wait instead of
 * reaching the server with a token issued for the new cookie. The rejected
 * request itself is never replayed: the user decides whether to submit again.
 */
const epoch = ref(0)
let epochEnded = new AbortController()

export const sessionEpoch = readonly(epoch)

export function advanceSessionEpoch(): void {
  epoch.value += 1
  epochEnded.abort()
  epochEnded = new AbortController()
}

/** Aborts when the current session epoch ends, so long-lived work started in it can stop. */
export function sessionEpochSignal(): AbortSignal {
  return epochEnded.signal
}

let rejections = 0
let confirmedRejections = 0
let confirmSession: (() => Promise<void>) | null = null
let confirming: Promise<boolean> | null = null

/** Installs how the current actor is read again; a rejected promise leaves the session unconfirmed. */
export function registerSessionConfirmation(confirm: () => Promise<void>): void {
  confirmSession = confirm
}

/** The server rejected the session or its CSRF token: cached tokens are stale and the actor must be read again. */
export function reportSessionRejected(): void {
  rejections += 1
  if (confirmSession) void confirmCurrentSession(confirmSession)
}

/** Changes whenever a cached CSRF token may belong to another session. */
export function sessionCredentialVersion(): string {
  return `${epoch.value}:${rejections}`
}

function confirmCurrentSession(confirm: () => Promise<void>): Promise<boolean> {
  if (confirming) return confirming
  // A rejection reported while this read is in flight may come from a newer
  // cookie, so this read only confirms the rejections that preceded it.
  const covered = rejections
  confirming = confirm()
    .then(
      () => {
        confirmedRejections = Math.max(confirmedRejections, covered)
        return true
      },
      () => false,
    )
    .finally(() => {
      confirming = null
    })
  return confirming
}

/** Resolves false when the actor could not be read again; the caller must not send its request. */
export async function awaitConfirmedSession(): Promise<boolean> {
  while (confirmSession && confirmedRejections < rejections) {
    if (!(await confirmCurrentSession(confirmSession))) return false
  }
  return true
}
