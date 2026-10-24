import { reactive, ref, watch, type Ref } from 'vue'
import { getProjectAgentCapability } from '@/generated/contracts'
import { diagnosticFromError, type DiagnosticViewModel } from '@/types/async'

/**
 * The project's Agent configuration as Control reports it. Work creation and
 * connection never depend on it; it only decides whether Agent entry points
 * are offered and which reason is shown when they are not. `configured` means
 * the project policy matches the deployment's model binding, not that a model
 * call or Credits admission will succeed.
 */
export type AgentCapabilityState =
  | { kind: 'idle' }
  | { kind: 'loading' }
  | { kind: 'configured'; model: string }
  /** No project AI setting: Agent features are off, everything else works. */
  | { kind: 'unconfigured'; reasonCode: string }
  /** A setting exists but its model binding is not usable in this deployment. */
  | { kind: 'unavailable'; reasonCode: string }
  /** The configuration could not be read (for example LW_AGENT_CONFIGURATION_UNREADABLE); Agent availability is unknown. */
  | { kind: 'unreadable'; diagnostic: DiagnosticViewModel }

const REASONS: Record<string, string> = {
  LW_AGENT_MODEL_NOT_CONFIGURED: '项目还没有配置模型',
  LW_AGENT_MODEL_BINDING_UNAVAILABLE: '项目配置的模型在当前平台部署中不可用',
}

/** Chinese reason for an `unconfigured` or `unavailable` capability. */
export function agentCapabilityReason(state: Extract<AgentCapabilityState, { reasonCode: string }>): string {
  return REASONS[state.reasonCode] ?? (state.kind === 'unconfigured' ? '项目还没有配置模型' : '项目配置的模型当前不可用')
}

export function useProjectAgentCapability(projectId: Ref<string | null | undefined>) {
  const state = ref<AgentCapabilityState>({ kind: 'idle' })
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
      const result = await getProjectAgentCapability({ path: { projectId: id } })
      if (current !== generation) return
      if (result.data) {
        const data = result.data
        state.value = data.status === 'configured'
          ? { kind: 'configured', model: data.model }
          : { kind: data.status, reasonCode: data.reasonCode }
        return
      }
      state.value = {
        kind: 'unreadable',
        diagnostic: diagnosticFromError(result.error, 'AGENT_CAPABILITY_UNREADABLE', '无法读取项目的 Agent 配置。'),
      }
    } catch (error) {
      if (current !== generation) return
      state.value = {
        kind: 'unreadable',
        diagnostic: diagnosticFromError(error, 'AGENT_CAPABILITY_UNREADABLE', '无法读取项目的 Agent 配置。'),
      }
    }
  }

  watch(projectId, load, { immediate: true })
  return reactive({ state, load })
}
