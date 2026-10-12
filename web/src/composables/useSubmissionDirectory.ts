import { reactive, ref, watch, type Ref } from 'vue'
import { getEnvironment, getFrozenSubmission } from '@/generated/contracts'
import { extractProblemDetails } from '@/types/async'

/**
 * What the server confirms about one frozen submission: when it was saved and
 * which of the student's environments it came from.
 */
export type SubmissionRecord =
  | { kind: 'loading' }
  | { kind: 'ready'; frozenAt: string; environmentId: string; environmentLabel: string | null }
  /** The server no longer returns this submission to this account. */
  | { kind: 'gone' }
  | { kind: 'error' }

/** A submission this browser started; only identifiers are stored, never state. */
export interface TrackedSubmission {
  submissionId: string
  environmentId: string
  submittedAt: string
  /** When this browser started tracking it; used only to expire old entries. */
  trackedAt?: string
}

const STORAGE_PREFIX = 'labweaver_submissions:'
const MAX_TRACKED = 20
const TRACK_DAYS = 30

function storageKey(projectId: string) {
  return `${STORAGE_PREFIX}${projectId}`
}

function isTracked(value: unknown): value is TrackedSubmission {
  if (!value || typeof value !== 'object') return false
  const item = value as Record<string, unknown>
  return typeof item.submissionId === 'string' && typeof item.environmentId === 'string' && typeof item.submittedAt === 'string'
}

export function readTrackedSubmissions(projectId: string): TrackedSubmission[] {
  if (typeof localStorage === 'undefined') return []
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(storageKey(projectId)) ?? '[]')
    if (!Array.isArray(parsed)) return []
    const cutoff = Date.now() - TRACK_DAYS * 24 * 60 * 60 * 1000
    return parsed.filter(isTracked).filter((item) => Date.parse(item.trackedAt ?? item.submittedAt) >= cutoff)
  } catch {
    return []
  }
}

function writeTrackedSubmissions(projectId: string, items: TrackedSubmission[]) {
  if (typeof localStorage === 'undefined') return
  try {
    localStorage.setItem(storageKey(projectId), JSON.stringify(items.slice(0, MAX_TRACKED)))
  } catch {
    // Storage full or disabled: tracking stays best-effort; results still come from the server.
  }
}

/** Remember a submission so leaving and returning still shows its progress. */
export function rememberSubmission(projectId: string, entry: TrackedSubmission) {
  const items = readTrackedSubmissions(projectId).filter((item) => item.submissionId !== entry.submissionId)
  writeTrackedSubmissions(projectId, [{ ...entry, trackedAt: new Date().toISOString() }, ...items])
}

export function forgetSubmission(projectId: string, submissionId: string) {
  writeTrackedSubmissions(projectId, readTrackedSubmissions(projectId).filter((item) => item.submissionId !== submissionId))
}

/**
 * Resolves frozen submissions to their saved time and environment name for the
 * current project. Records are cached per project and re-read only on demand.
 */
export function useSubmissionDirectory(projectId: Ref<string | undefined>) {
  const records = ref<Record<string, SubmissionRecord>>({})
  const environmentLabels = new Map<string, Promise<string | null>>()
  let generation = 0

  function environmentLabel(environmentId: string): Promise<string | null> {
    let pending = environmentLabels.get(environmentId)
    if (!pending) {
      pending = getEnvironment({ path: { environmentId } })
        .then((response) => (response.data?.displayLabel?.trim() || null))
        .catch(() => null)
      environmentLabels.set(environmentId, pending)
    }
    return pending
  }

  async function resolveOne(project: string, submissionId: string, current: number) {
    records.value = { ...records.value, [submissionId]: { kind: 'loading' } }
    let record: SubmissionRecord
    try {
      const response = await getFrozenSubmission({ path: { projectId: project, submissionId } })
      if (response.data && response.data.projectId === project && response.data.id === submissionId) {
        const environmentId = response.data.environment.environmentId
        record = {
          kind: 'ready',
          frozenAt: response.data.frozenAt,
          environmentId,
          environmentLabel: await environmentLabel(environmentId),
        }
      } else {
        const status = extractProblemDetails(response.error)?.status
        record = status === 403 || status === 404 ? { kind: 'gone' } : { kind: 'error' }
      }
    } catch {
      record = { kind: 'error' }
    }
    if (current !== generation || projectId.value !== project) return
    records.value = { ...records.value, [submissionId]: record }
  }

  /** Resolve submissions not yet known; failed ones are retried. */
  async function resolve(submissionIds: readonly string[]) {
    const project = projectId.value
    if (!project) return
    const current = generation
    const missing = [...new Set(submissionIds)].filter((id) => {
      const record = records.value[id]
      return !record || record.kind === 'error'
    })
    // Small batches keep the browser from flooding the API on a long list.
    for (let index = 0; index < missing.length; index += 4) {
      await Promise.all(missing.slice(index, index + 4).map((id) => resolveOne(project, id, current)))
      if (current !== generation) return
    }
  }

  watch(projectId, () => {
    generation += 1
    records.value = {}
    environmentLabels.clear()
  })

  return reactive({ records, resolve })
}
