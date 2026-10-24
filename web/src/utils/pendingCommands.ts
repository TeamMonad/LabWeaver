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

/** Returns false when the command could not be persisted (storage disabled or quota exhausted). */
export function writePendingCommand<T>(actorId: string, scope: string, command: PendingCommand<T>): boolean {
  try {
    const storage = store()
    if (!storage) return false
    storage.setItem(entryKey(actorId, scope), JSON.stringify(command))
    return true
  } catch {
    // Storage unavailable (private mode, quota): the command still runs once in memory.
    return false
  }
}

/**
 * Remove the command only while it is still the one identified by
 * `idempotencyKey`: a late answer to an older command must not delete a newer
 * command that replaced it.
 */
export function clearPendingCommand(actorId: string, scope: string, idempotencyKey: string) {
  try {
    if (readPendingCommand(actorId, scope)?.idempotencyKey === idempotencyKey) store()?.removeItem(entryKey(actorId, scope))
  } catch {
    // Nothing was stored either.
  }
}

/**
 * Every pending command of `actorId` whose scope starts with `scopePrefix`,
 * for pages that keep several unconfirmed commands of one kind side by side
 * (one scope per command identity) and must offer all of them after a refresh.
 */
export function listPendingCommands<T>(actorId: string, scopePrefix: string): Array<{ scope: string; command: PendingCommand<T> }> {
  const storage = store()
  if (!storage) return []
  const prefix = entryKey(actorId, '')
  const entries: Array<{ scope: string; command: PendingCommand<T> }> = []
  try {
    for (let index = 0; index < storage.length; index += 1) {
      const key = storage.key(index)
      if (!key?.startsWith(entryKey(actorId, scopePrefix))) continue
      const scope = key.slice(prefix.length)
      const command = readPendingCommand<T>(actorId, scope)
      if (command) entries.push({ scope, command })
    }
  } catch {
    // Storage disabled: nothing was stored either.
  }
  return entries.sort((left, right) => left.scope.localeCompare(right.scope))
}

/**
 * Remove every pending command of `actorId` whose scope starts with
 * `scopePrefix`. Only for a command kind the page can no longer send: no newer
 * command of that kind can exist, so nothing has to be compared first.
 */
export function clearPendingCommandsWithScopePrefix(actorId: string, scopePrefix: string) {
  const storage = store()
  if (!storage) return
  try {
    const prefix = entryKey(actorId, scopePrefix)
    const stale: string[] = []
    for (let index = 0; index < storage.length; index += 1) {
      const key = storage.key(index)
      if (key?.startsWith(prefix)) stale.push(key)
    }
    for (const key of stale) storage.removeItem(key)
  } catch {
    // Storage disabled: nothing was stored either.
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
