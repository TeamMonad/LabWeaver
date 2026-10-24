/**
 * Unconfirmed write commands that must survive a page refresh.
 *
 * A command whose response was lost (timeout, closed tab) may already have
 * been applied by the server. Keeping its Idempotency-Key and identity in
 * session storage lets the next attempt with the same content replay that
 * command instead of creating a second resource. When session storage refuses
 * a write (disabled, quota exhausted) the command is kept in page memory
 * instead, so retries in this page still reuse its key; it is then lost on a
 * refresh. Entries belong to one actor: signing in as someone else or signing
 * out removes every entry of the previous actor, stored or in memory
 * (`syncPendingCommandOwner`, called by `useAuth`).
 */
const PREFIX = 'labweaver.pending-command:'

export interface PendingCommand<T = unknown> {
  /** Canonical content of the command; a different fingerprint is a different command. */
  fingerprint: string
  idempotencyKey: string
  /** Command-specific state restored with it (for example the reviewed draft). */
  data: T
}

export interface ListedPendingCommand<T> {
  scope: string
  command: PendingCommand<T>
  /** False when only page memory holds it: a refresh loses the command and its key. */
  persisted: boolean
}

/** Commands session storage refused, keyed like their storage entry. */
const unpersisted = new Map<string, PendingCommand<unknown>>()

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

function readStored<T>(key: string): PendingCommand<T> | null {
  try {
    const raw = store()?.getItem(key)
    if (!raw) return null
    const parsed = JSON.parse(raw) as Partial<PendingCommand<T>>
    if (typeof parsed.fingerprint !== 'string' || typeof parsed.idempotencyKey !== 'string' || !parsed.idempotencyKey) return null
    return parsed as PendingCommand<T>
  } catch {
    return null
  }
}

/** Storage keys of every stored entry starting with `prefix`; empty when storage is unavailable. */
function storedKeys(prefix: string): string[] {
  const storage = store()
  if (!storage) return []
  const keys: string[] = []
  try {
    for (let index = 0; index < storage.length; index += 1) {
      const key = storage.key(index)
      if (key?.startsWith(prefix)) keys.push(key)
    }
  } catch {
    // Storage disabled: nothing was stored either.
  }
  return keys
}

function removeStored(keys: string[]) {
  try {
    for (const key of keys) store()?.removeItem(key)
  } catch {
    // Storage disabled: nothing was stored either.
  }
}

export function readPendingCommand<T>(actorId: string, scope: string): PendingCommand<T> | null {
  const key = entryKey(actorId, scope)
  return readStored<T>(key) ?? (unpersisted.get(key) as PendingCommand<T> | undefined) ?? null
}

/** Returns false when the command could not be persisted and is held in page memory only. */
export function writePendingCommand<T>(actorId: string, scope: string, command: PendingCommand<T>): boolean {
  const key = entryKey(actorId, scope)
  try {
    const storage = store()
    if (storage) {
      storage.setItem(key, JSON.stringify(command))
      unpersisted.delete(key)
      return true
    }
  } catch {
    // Storage unavailable or full: keep the key in memory below.
  }
  unpersisted.set(key, command)
  return false
}

/**
 * Remove the command only while it is still the one identified by
 * `idempotencyKey`: a late answer to an older command must not delete a newer
 * command that replaced it.
 */
export function clearPendingCommand(actorId: string, scope: string, idempotencyKey: string) {
  const key = entryKey(actorId, scope)
  if (readStored(key)?.idempotencyKey === idempotencyKey) removeStored([key])
  if (unpersisted.get(key)?.idempotencyKey === idempotencyKey) unpersisted.delete(key)
}

/**
 * Every pending command of `actorId` whose scope starts with `scopePrefix`,
 * for pages that keep several unconfirmed commands of one kind side by side
 * (one scope per command identity) and must offer all of them after a refresh.
 */
export function listPendingCommands<T>(actorId: string, scopePrefix: string): Array<ListedPendingCommand<T>> {
  const actorPrefix = entryKey(actorId, '')
  const prefix = entryKey(actorId, scopePrefix)
  const entries: Array<ListedPendingCommand<T>> = []
  const seen = new Set<string>()
  for (const key of storedKeys(prefix)) {
    const command = readStored<T>(key)
    if (!command) continue
    seen.add(key)
    entries.push({ scope: key.slice(actorPrefix.length), command, persisted: true })
  }
  for (const [key, command] of unpersisted) {
    if (!key.startsWith(prefix) || seen.has(key)) continue
    entries.push({ scope: key.slice(actorPrefix.length), command: command as PendingCommand<T>, persisted: false })
  }
  return entries.sort((left, right) => left.scope.localeCompare(right.scope))
}

/**
 * Remove every pending command of `actorId` whose scope starts with
 * `scopePrefix`. Only for a command kind the page can no longer send: no newer
 * command of that kind can exist, so nothing has to be compared first.
 */
export function clearPendingCommandsWithScopePrefix(actorId: string, scopePrefix: string) {
  const prefix = entryKey(actorId, scopePrefix)
  removeStored(storedKeys(prefix))
  for (const key of [...unpersisted.keys()]) if (key.startsWith(prefix)) unpersisted.delete(key)
}

/** Remove every pending command, stored or in memory, that does not belong to `actorId` (all of them on sign-out). */
export function syncPendingCommandOwner(actorId: string | null) {
  const keep = actorId ? entryKey(actorId, '') : null
  const foreign = (key: string) => !keep || !key.startsWith(keep)
  removeStored(storedKeys(PREFIX).filter(foreign))
  for (const key of [...unpersisted.keys()]) if (foreign(key)) unpersisted.delete(key)
}
