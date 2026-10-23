/**
 * Submission identifiers this browser started, kept per signed-in actor and
 * project so leaving and returning still finds them. Only identifiers are
 * stored: names, times and states are shown only after the server returns
 * them to the current actor. One actor owns the stored list at a time;
 * signing out or signing in as someone else discards the previous list.
 */
const STORAGE_PREFIX = 'labweaver_submissions:'
const OWNER_KEY = 'labweaver_submissions_owner'
const MAX_TRACKED = 20

type TrackedStore = Record<string, string[]>

function storage(): Storage | null {
  return typeof localStorage === 'undefined' ? null : localStorage
}

function storageKey(actorId: string) {
  return `${STORAGE_PREFIX}${actorId}`
}

function readStore(actorId: string): TrackedStore {
  try {
    const parsed: unknown = JSON.parse(storage()?.getItem(storageKey(actorId)) ?? '{}')
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {}
    const store: TrackedStore = {}
    for (const [projectId, ids] of Object.entries(parsed)) {
      if (!Array.isArray(ids)) continue
      const valid = ids.filter((id): id is string => typeof id === 'string' && id.length > 0).slice(0, MAX_TRACKED)
      if (valid.length) store[projectId] = valid
    }
    return store
  } catch {
    return {}
  }
}

function writeStore(actorId: string, store: TrackedStore) {
  try {
    if (Object.keys(store).length) storage()?.setItem(storageKey(actorId), JSON.stringify(store))
    else storage()?.removeItem(storageKey(actorId))
  } catch {
    // Storage full or disabled: tracking stays best-effort; results still come from the server.
  }
}

/** Submission identifiers of the actor in one project, most recent first. */
export function readTrackedSubmissionIds(actorId: string | null | undefined, projectId: string | null | undefined): string[] {
  if (!actorId || !projectId) return []
  return readStore(actorId)[projectId] ?? []
}

/** Remember a submission the actor just made. */
export function rememberSubmission(actorId: string | null | undefined, projectId: string, submissionId: string) {
  if (!actorId) return
  syncTrackedSubmissionOwner(actorId)
  const store = readStore(actorId)
  store[projectId] = [submissionId, ...(store[projectId] ?? []).filter((id) => id !== submissionId)].slice(0, MAX_TRACKED)
  writeStore(actorId, store)
}

/** Stop tracking a submission: it has a result, or the server no longer returns it to this actor. */
export function forgetSubmission(actorId: string | null | undefined, projectId: string, submissionId: string) {
  if (!actorId) return
  const store = readStore(actorId)
  const remaining = (store[projectId] ?? []).filter((id) => id !== submissionId)
  if (remaining.length) store[projectId] = remaining
  else delete store[projectId]
  writeStore(actorId, store)
}

/** Records the signed-in actor; `null` means signed out. A different or absent actor discards the stored list. */
export function syncTrackedSubmissionOwner(actorId: string | null) {
  const store = storage()
  if (!store) return
  try {
    const owner = store.getItem(OWNER_KEY)
    if (owner && owner !== actorId) store.removeItem(storageKey(owner))
    if (actorId) store.setItem(OWNER_KEY, actorId)
    else store.removeItem(OWNER_KEY)
  } catch {
    // Storage disabled: nothing was stored either.
  }
}
