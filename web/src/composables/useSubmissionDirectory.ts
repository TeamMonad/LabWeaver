import { computed, reactive, ref, watch, type Ref } from 'vue'
import { getEnvironment, getFrozenSubmission } from '@/generated/contracts'
import { useAuth } from '@/composables/useAuth'
import { extractProblemDetails } from '@/types/async'
import { actorIdFromProfile } from '@/utils/navigation'
import { forgetSubmission, readTrackedSubmissionIds } from '@/utils/trackedSubmissions'

/**
 * What the server confirms to the current actor about one frozen submission:
 * when it was saved and which environment it came from.
 */
export type SubmissionRecord =
  | { kind: 'loading' }
  | { kind: 'ready'; frozenAt: string; environmentId: string; environmentLabel: string | null }
  /** The server does not return this submission to the current actor. */
  | { kind: 'gone' }
  | { kind: 'error' }

/** The signed-in actor; tracked submissions and their server records belong to it. */
export function useCurrentActorId() {
  const auth = useAuth()
  return computed(() => (auth.isAuthenticated.value ? actorIdFromProfile(auth.user.value?.profile) : null))
}

/**
 * Submissions the current actor started in this browser for one project, and
 * what the server confirms about each. Only identifiers come from the browser;
 * a submission counts only after the server returns it to this actor, and one
 * the server refuses (403/404) or attributes to someone else is dropped.
 */
export function useSubmissionDirectory(projectId: Ref<string | undefined>) {
  const actorId = useCurrentActorId()
  const records = ref<Record<string, SubmissionRecord>>({})
  const tracked = ref<string[]>([])
  const environmentLabels = new Map<string, Promise<string | null>>()
  let generation = 0

  /** Re-read the identifiers stored for the current actor and project. */
  function reload() {
    tracked.value = readTrackedSubmissionIds(actorId.value, projectId.value)
  }

  /** Stop tracking a submission; its server record, if any, stays readable. */
  function forget(submissionId: string) {
    const project = projectId.value
    if (!project) return
    forgetSubmission(actorId.value, project, submissionId)
    reload()
  }

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

  async function resolveOne(project: string, actor: string, submissionId: string, current: number) {
    records.value = { ...records.value, [submissionId]: { kind: 'loading' } }
    let record: SubmissionRecord
    try {
      const response = await getFrozenSubmission({ path: { projectId: project, submissionId } })
      const data = response.data
      if (data && data.projectId === project && data.id === submissionId && data.actorId === actor) {
        record = {
          kind: 'ready',
          frozenAt: data.frozenAt,
          environmentId: data.environment.environmentId,
          environmentLabel: await environmentLabel(data.environment.environmentId),
        }
      } else if (data) {
        // Another project, submission or actor: never shown, never tracked.
        record = { kind: 'gone' }
      } else {
        const status = extractProblemDetails(response.error)?.status
        record = status === 403 || status === 404 ? { kind: 'gone' } : { kind: 'error' }
      }
    } catch {
      record = { kind: 'error' }
    }
    if (current !== generation) return
    records.value = { ...records.value, [submissionId]: record }
    if (record.kind === 'gone') forget(submissionId)
  }

  /** Resolve submissions not yet confirmed; failed reads are retried. */
  async function resolve(submissionIds: readonly string[]) {
    const project = projectId.value
    const actor = actorId.value
    if (!project || !actor) return
    const current = generation
    const missing = [...new Set(submissionIds)].filter((id) => {
      const record = records.value[id]
      return !record || record.kind === 'error'
    })
    // Small batches keep the browser from flooding the API on a long list.
    for (let index = 0; index < missing.length; index += 4) {
      await Promise.all(missing.slice(index, index + 4).map((id) => resolveOne(project, actor, id, current)))
      if (current !== generation) return
    }
  }

  // A different project or actor starts from nothing: no record carries over.
  watch([projectId, actorId], () => {
    generation += 1
    records.value = {}
    environmentLabels.clear()
    reload()
  }, { immediate: true })

  return reactive({ records, tracked, resolve, forget, reload })
}
