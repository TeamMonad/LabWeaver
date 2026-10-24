/**
 * Unconfirmed write commands that must survive a page refresh.
 *
 * A command whose response was lost (timeout, closed tab) may already have
 * been applied by the server. Keeping its Idempotency-Key and identity in
 * session storage lets the next attempt with the same content replay that
 * command instead of creating a second resource. Entries belong to one actor:
 * signing in as someone else or signing out removes every entry of the
 * previous actor (`syncPendingCommandOwner`, called by `useAuth`).
 */
const PREFIX = 'labweaver.pending-command:'

export interface PendingCommand<T = unknown> {
  /** Canonical content of the command; a different fingerprint is a different command. */
  fingerprint: string
  idempotencyKey: string
  /** Command-specific state restored with it (for example the reviewed draft). */
  data: T
}

function store(): Storage | null {
  try {
    return typeof window === 'undefined' ? null : window.sessionStorage
  } catch {
    return null
  }
}

function entryKey(actorId: string, scope: string): string {
  return `${PREFIX}${actorId}:${scope}`
}

export function readPendingCommand<T>(actorId: string, scope: string): PendingCommand<T> | null {
  try {
    const raw = store()?.getItem(entryKey(actorId, scope))
    if (!raw) return null
    const parsed = JSON.parse(raw) as Partial<PendingCommand<T>>
    if (typeof parsed.fingerprint !== 'string' || typeof parsed.idempotencyKey !== 'string' || !parsed.idempotencyKey) return null
    return parsed as PendingCommand<T>
  } catch {
    return null
  }
}

export function writePendingCommand<T>(actorId: string, scope: string, command: PendingCommand<T>) {
  try {
    store()?.setItem(entryKey(actorId, scope), JSON.stringify(command))
  } catch {
    // Storage unavailable (private mode, quota): the command still runs once in memory.
  }
}

export function clearPendingCommand(actorId: string, scope: string) {
  try {
    store()?.removeItem(entryKey(actorId, scope))
  } catch {
    // Nothing was stored either.
  }
}

/** Remove every pending command that does not belong to `actorId` (all of them on sign-out). */
export function syncPendingCommandOwner(actorId: string | null) {
  const storage = store()
  if (!storage) return
  try {
    const keep = actorId ? `${PREFIX}${actorId}:` : null
    const stale: string[] = []
    for (let index = 0; index < storage.length; index += 1) {
      const key = storage.key(index)
      if (key?.startsWith(PREFIX) && (!keep || !key.startsWith(keep))) stale.push(key)
    }
    for (const key of stale) storage.removeItem(key)
  } catch {
    // Storage disabled: nothing was stored either.
  }
}

/**
 * Whether a failed command has a definite outcome: the server answered with a
 * client error that a replay would repeat. Timeouts, throttling and server or
 * transport failures leave the outcome unknown, so the command is kept.
 */
export function commandOutcomeIsFinal(status: number | undefined): boolean {
  return status !== undefined && status >= 400 && status < 500 && status !== 408 && status !== 429
}
