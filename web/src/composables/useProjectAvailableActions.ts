import { computed, onScopeDispose, ref, watch, type Ref } from 'vue'
import {
  getProjectAvailableActions,
  type AvailableObjectActionsSchema,
  type ObjectAction,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'

function diagnostic(error: unknown, fallbackCode: string, fallbackMessage: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackMessage, problem?.retryable ?? true)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

const OBJECT_ACTIONS: ReadonlySet<string> = new Set([
  'browse',
  'edit',
  'start',
  'stop',
  'configure_agent',
  'grant_access',
  'manage_members',
  'view_costs',
  'request_runtime_permissions',
  'approve_runtime_permissions',
  'manage_runtime_policy',
])

function asAvailableActions(value: unknown): AvailableObjectActionsSchema {
  if (
    !isRecord(value)
    || typeof value.projectId !== 'string'
    || typeof value.actorId !== 'string'
    || typeof value.membershipRevision !== 'number'
    || !Number.isSafeInteger(value.membershipRevision)
    || typeof value.validUntil !== 'string'
    || !Array.isArray(value.actions)
    || !value.actions.every((action) => typeof action === 'string' && OBJECT_ACTIONS.has(action))
  ) {
    throw new Error('项目可用动作返回了无法识别的数据。')
  }
  return value as AvailableObjectActionsSchema
}

export function useProjectAvailableActions(projectId: Ref<string | null | undefined>) {
  const state = ref<AsyncState<AvailableObjectActionsSchema>>({ kind: 'idle' })
  let loadGeneration = 0
  let disposed = false

  onScopeDispose(() => {
    disposed = true
  })

  async function load() {
    const id = projectId.value?.trim() || null
    const generation = ++loadGeneration
    if (!id) {
      state.value = { kind: 'idle' }
      return
    }
    state.value = { kind: 'loading', message: '加载项目可用动作…' }
    try {
      const result = await getProjectAvailableActions({ path: { projectId: id } })
      if (disposed || generation !== loadGeneration || projectId.value !== id) return
      if (result.error) throw result.error
      const data = asAvailableActions(result.data)
      state.value = { kind: 'success', data }
    } catch (error) {
      if (disposed || generation !== loadGeneration) return
      state.value = {
        kind: 'error',
        diagnostic: diagnostic(error, 'PROJECT_AVAILABLE_ACTIONS_LOAD_FAILED', '无法加载项目可用动作；写操作按钮将保持关闭，直至刷新成功。'),
      }
    }
  }

  function allows(action: ObjectAction): boolean {
    return state.value.kind === 'success' && state.value.data.actions.includes(action)
  }

  const actions = computed<ObjectAction[]>(() => (
    state.value.kind === 'success' ? state.value.data.actions : []
  ))

  const canStart = computed(() => allows('start'))
  const canStop = computed(() => allows('stop'))
  const canEdit = computed(() => allows('edit'))
  const canManageMembers = computed(() => allows('manage_members'))
  const canViewCosts = computed(() => allows('view_costs'))
  const canConfigureAgent = computed(() => allows('configure_agent'))
  const canGrantAccess = computed(() => allows('grant_access'))

  watch(projectId, () => {
    void load()
  }, { immediate: true })

  return {
    state,
    actions,
    allows,
    canStart,
    canStop,
    canEdit,
    canManageMembers,
    canViewCosts,
    canConfigureAgent,
    canGrantAccess,
    load,
  }
}

export type { ObjectAction, AvailableObjectActionsSchema }
