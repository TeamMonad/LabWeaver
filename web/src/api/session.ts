import { readonly, ref } from 'vue'

/**
 * Which browser session API calls belong to.
 *
 * The BFF session cookie is HttpOnly and shared by every tab, so another tab
 * can sign out or sign in as someone else without this one noticing. The epoch
 * advances when the signed-in actor changes or the session is confirmed ended;
 * the API transport discards the result of any request started under an older
 * epoch, streams opened in it stop, and the AppShell re-creates the routed page
 * so no form or list of the previous actor survives.
 *
 * Writes are bound to their actor by the server: a CSRF token is valid only
 * for the session that issued it, and the transport sends a write only with a
 * token issued to the actor the request started with. A CSRF or session
 * rejection retires cached tokens and reads the actor again; the rejected
 * request itself is never replayed, the user decides whether to submit again.
 */
const epoch = ref(0)
let epochEnded = new AbortController()
// The actor the current epoch belongs to: undefined until an identity is first
// known, null after a confirmed sign-out.
let epochActor: string | null | undefined

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

/**
 * Records who the browser session belongs to: an actor id, or null once the
 * session is confirmed ended. Another actor, or a sign-out after an actor was
 * known, ends the epoch; the first identity of the page does not.
 */
export function recordSessionActor(actorId: string | null): void {
  const ended = actorId === null ? Boolean(epochActor) : epochActor !== undefined && epochActor !== actorId
  if (ended) advanceSessionEpoch()
  epochActor = actorId
}
/** The actor the current epoch belongs to, when it is known. */
export function sessionActor(): string | undefined {
  return epochActor ?? undefined
}

let rejections = 0
let confirmSession: (() => Promise<void>) | null = null
let confirming: Promise<void> | null = null

/** Installs how the current actor is read again. */
export function registerSessionConfirmation(confirm: () => Promise<void>): void {
  confirmSession = confirm
}

/** The server rejected the session or its CSRF token: cached tokens are stale and the actor is read again. */
export function reportSessionRejected(): void {
  rejections += 1
  if (!confirmSession || confirming) return
  confirming = confirmSession()
    // A failed read keeps the current identity; the next rejection reads again.
    .catch(() => undefined)
    .finally(() => {
      confirming = null
    })
}

/** Changes whenever a cached CSRF token may no longer match the session cookie. */
export function sessionCredentialVersion(): string {
  return `${epoch.value}:${rejections}`
}
