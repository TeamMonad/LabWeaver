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
 * token issued to the actor the request started with, only once no identity
 * read is in flight (within the write's own timeout and cancellation) and only
 * while the actor is known. A CSRF or session rejection puts the actor in doubt
 * at once (pages that act for it close), retires cached tokens and reads the
 * actor again; while the actor stays unknown writes are refused unsent. The
 * rejected request itself is never replayed, the user decides whether to
 * submit again.
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

// From the moment the server rejects this tab's session or CSRF token, or an
// identity read fails, until an identity read answers, the current actor is in
// doubt: pages that act for it stay closed and writes are refused unsent.
// Neither the epoch nor the actor's unconfirmed commands change: the read
// that answers decides that.
const actorInDoubt = ref(false)
const confirmingActor = ref(false)
// Rejections counted so far, and how many of them the latest answer covers.
let rejectionGeneration = 0
let answeredGeneration = 0
let answers = 0

/** Whether the actor is unknown: rejected or unreadable, and no identity read has answered since. */
export const sessionActorInDoubt = readonly(actorInDoubt)
/** Whether the identity read that follows a rejection is still in flight. */
export const sessionActorConfirming = readonly(confirmingActor)

/** An identity read answered (with an actor or a confirmed sign-out): the doubt is resolved. */
export function resolveSessionActorDoubt(): void {
  actorInDoubt.value = false
  answeredGeneration = rejectionGeneration
  answers += 1
}

/** An identity read failed (transport error, timeout): the actor is unknown until a read answers. */
export function markSessionActorUnknown(): void {
  actorInDoubt.value = true
}

// The latest identity read decides; waiters are released when it settles,
// including those that started waiting on a read it superseded.
let identityRead: Promise<unknown> | null = null
const identityWaiters = new Set<() => void>()

/** Records the identity read now in flight; writes wait for it before they are sent. */
export function trackIdentityRead(read: Promise<unknown>): void {
  identityRead = read
  const settled = () => {
    if (identityRead !== read) return
    identityRead = null
    for (const release of [...identityWaiters]) release()
  }
  read.then(settled, settled)
}

/** Whether an identity read is in flight right now (the final write check refuses then). */
export function identityReadPending(): boolean {
  return identityRead !== null
}

/** Why waiting for the identity read ended before it settled. */
export class IdentityWaitInterrupted extends Error {
  constructor(readonly reason: 'timeout' | 'cancelled') {
    super(reason === 'timeout' ? 'Timed out waiting for the identity read.' : 'Cancelled while waiting for the identity read.')
  }
}

interface IdentityWaitOptions {
  /** Stop waiting when the caller cancels; rejects with `IdentityWaitInterrupted('cancelled')`. */
  signal?: { aborted: boolean; addEventListener?: (type: 'abort', listener: () => void) => void; removeEventListener?: (type: 'abort', listener: () => void) => void }
  /** Stop waiting after this many milliseconds; rejects with `IdentityWaitInterrupted('timeout')`. */
  timeoutMs?: number
}

/**
 * Resolves once no identity read is in flight, whatever the reads answered.
 * A caller's cancellation or timeout ends the wait early with
 * `IdentityWaitInterrupted`, so nothing waits on a read that never answers.
 */
export function identityReadsSettled({ signal, timeoutMs }: IdentityWaitOptions = {}): Promise<void> {
  if (!identityRead) return Promise.resolve()
  if (signal?.aborted) return Promise.reject(new IdentityWaitInterrupted('cancelled'))
  // `Promise.withResolvers` is outside the project's ES2020 lib (and Node 20): the executor form captures the resolvers.
  return new Promise<void>((resolve, reject) => {
    let timer: ReturnType<typeof setTimeout> | undefined
    const finish = () => {
      identityWaiters.delete(release)
      signal?.removeEventListener?.('abort', cancel)
      clearTimeout(timer)
    }
    const release = () => {
      finish()
      resolve()
    }
    const cancel = () => {
      finish()
      reject(new IdentityWaitInterrupted('cancelled'))
    }
    identityWaiters.add(release)
    signal?.addEventListener?.('abort', cancel)
    if (timeoutMs !== undefined && timeoutMs > 0) {
      timer = setTimeout(() => {
        finish()
        reject(new IdentityWaitInterrupted('timeout'))
      }, timeoutMs)
    }
  })
}

/** Installs how the current actor is read again. */
export function registerSessionConfirmation(confirm: () => Promise<void>): void {
  confirmSession = confirm
}

function startConfirmation(confirm: () => Promise<void>): void {
  const answersBefore = answers
  confirmingActor.value = true
  confirming = confirm()
    // A failed read keeps the identity in doubt; the next rejection or read asks again.
    .catch(() => undefined)
    .finally(() => {
      confirming = null
      // A rejection that arrived after this read answered is not covered by
      // that answer: read again. Rejections while it was in flight are.
      if (answers !== answersBefore && rejectionGeneration !== answeredGeneration && confirmSession) {
        startConfirmation(confirmSession)
        return
      }
      confirmingActor.value = false
    })
}

/** The server rejected the session or its CSRF token: cached tokens are stale and the actor is read again. */
export function reportSessionRejected(): void {
  rejections += 1
  if (!confirmSession) return
  rejectionGeneration += 1
  // Synchronously, so nothing renders for the actor before the read starts.
  actorInDoubt.value = true
  if (!confirming) startConfirmation(confirmSession)
}

/** Changes whenever a cached CSRF token may no longer match the session cookie. */
export function sessionCredentialVersion(): string {
  return `${epoch.value}:${rejections}`
}
