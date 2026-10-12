import { reactive, ref, watch, type Ref } from 'vue'
import { getActiveProjectLlmPolicy } from '@/generated/contracts'
import { extractProblemDetails } from '@/types/async'

/**
 * Whether the project's Agent features can run. Work creation and connection
 * never depend on this; it only decides whether Agent entry points are offered
 * and which reason is shown when they are not.
 */
export type AgentAvailability =
  | { kind: 'idle' }
  | { kind: 'loading' }
  | { kind: 'available'; model: string }
  /** No active project AI setting: Agent features are off, everything else works. */
  | { kind: 'unconfigured' }
  /** The setting could not be read; Agent availability is unknown. */
  | { kind: 'unknown'; code: string }

export function useAgentAvailability(projectId: Ref<string | null | undefined>) {
  const state = ref<AgentAvailability>({ kind: 'idle' })
  let generation = 0

  async function load() {
    const id = projectId.value
    const current = ++generation
    if (!id) {
      state.value = { kind: 'idle' }
      return
    }
    state.value = { kind: 'loading' }
    try {
      const result = await getActiveProjectLlmPolicy({ path: { projectId: id } })
      if (current !== generation) return
      if (result.data) {
        state.value = { kind: 'available', model: result.data.binding.model }
        return
      }
      const problem = extractProblemDetails(result.error)
      state.value = problem?.status === 404 || problem?.diagnosticCode === 'LW_LLM_POLICY_NOT_FOUND'
        ? { kind: 'unconfigured' }
        : { kind: 'unknown', code: problem?.diagnosticCode ?? 'AGENT_AVAILABILITY_UNKNOWN' }
    } catch {
      if (current === generation) state.value = { kind: 'unknown', code: 'AGENT_AVAILABILITY_UNKNOWN' }
    }
  }

  watch(projectId, load, { immediate: true })
  return reactive({ state, load })
}
