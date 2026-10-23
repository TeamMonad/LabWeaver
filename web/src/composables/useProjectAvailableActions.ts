import { computed, getCurrentScope, onScopeDispose, ref, watch, type ComputedRef, type Ref } from 'vue'
import {
  getProjectAvailableActions,
  type AvailableObjectActionsSchema,
  type ObjectAction,
} from '@/generated/contracts'
import { diagnosticFromError, makeDiagnostic, type AsyncState } from '@/types/async'

/** Chinese names of object actions, used in every permission reason. */
export const OBJECT_ACTION_LABELS: Record<ObjectAction, string> = {
  browse: '查看',
  archive: '归档',
  edit: '编辑',
  start: '启动',
  stop: '停止',
  configure_agent: '配置 Agent',
  grant_access: '签发访问授权',
  manage_members: '管理成员',
  view_costs: '查看费用',
  request_runtime_permissions: '申请运行权限',
  approve_runtime_permissions: '批准运行权限',
  manage_runtime_policy: '管理运行策略',
}

const OBJECT_ACTIONS: ReadonlySet<string> = new Set(Object.keys(OBJECT_ACTION_LABELS))

/** Read again shortly after the server-stated validity ends. */
const EXPIRY_SKEW_MS = 1000
/** An answer already past its validity (clock skew) is read again after a pause, not in a tight loop. */
const MIN_REFRESH_DELAY_MS = 5000
/** setTimeout stores its delay in a signed 32-bit integer. */
const MAX_TIMER_DELAY_MS = 2_147_483_647

function asAvailableActions(value: unknown, projectId: string): AvailableObjectActionsSchema | null {
  if (typeof value !== 'object' || value === null) return null
  const candidate = value as Partial<Record<keyof AvailableObjectActionsSchema, unknown>>
  if (
    candidate.projectId !== projectId
    || typeof candidate.actorId !== 'string'
    || typeof candidate.membershipRevision !== 'number'
    || !Number.isSafeInteger(candidate.membershipRevision)
    || typeof candidate.validUntil !== 'string'
    || Number.isNaN(Date.parse(candidate.validUntil))
    || !Array.isArray(candidate.actions)
    || !candidate.actions.every((action) => typeof action === 'string' && OBJECT_ACTIONS.has(action))
  ) {
    return null
  }
  return value as AvailableObjectActionsSchema
}

export interface ProjectAvailableActions {
  state: Ref<AsyncState<AvailableObjectActionsSchema>>
  /** The answer for the selected project while it is still valid; null otherwise. */
  current: ComputedRef<AvailableObjectActionsSchema | null>
  allows: (action: ObjectAction) => boolean
  /**
   * Why the page does not offer `action`, in Chinese; null when it is offered.
   * `purpose` names the user's task when it differs from the action's own name
   * (for example 提交评测, which the server checks as `edit`).
   */
  denialReason: (action: ObjectAction, purpose?: string) => string | null
  /** Read again; `silent` keeps the previous answer on screen until the new one arrives. */
  load: (silent?: boolean) => Promise<void>
  /** Membership may have changed (a write was refused, a member was added or removed): read again silently. */
  refresh: () => Promise<void>
}

/**
 * The current actor's object actions in one project, read from Control.
 *
 * The answer only decides what the page offers; every write is authorized again
 * by the server. Anything other than a current answer for this exact project
 * offers no action: while loading, after an error, for another project's
 * answer, and once `validUntil` has passed. The answer is read again when it
 * expires, when the page becomes visible and whenever a caller reports that
 * membership may have changed (`refresh`).
 */
export function useProjectAvailableActions(projectId: Ref<string | null | undefined>): ProjectAvailableActions {
  const state = ref<AsyncState<AvailableObjectActionsSchema>>({ kind: 'idle' })
  const now = ref(Date.now())
  let loadGeneration = 0
  let expiryTimer: ReturnType<typeof setTimeout> | undefined

  const pageVisible = () => typeof document === 'undefined' || document.visibilityState !== 'hidden'
  const currentId = () => projectId.value?.trim() || null

  async function load(silent = false) {
    const id = currentId()
    const generation = ++loadGeneration
    if (!id) {
      state.value = { kind: 'idle' }
      return
    }
    const previous = state.value
    if (!silent || previous.kind !== 'success' || previous.data.projectId !== id) {
      state.value = { kind: 'loading', message: '正在确认你在此项目的权限…' }
    }
    let result
    try {
      result = await getProjectAvailableActions({ path: { projectId: id } })
    } catch (error) {
      if (generation !== loadGeneration || currentId() !== id) return
      state.value = { kind: 'error', diagnostic: diagnosticFromError(error, 'PROJECT_AVAILABLE_ACTIONS_LOAD_FAILED', '无法确认你在此项目的权限。') }
      return
    }
    if (generation !== loadGeneration || currentId() !== id) return
    now.value = Date.now()
    if (result.error) {
      state.value = { kind: 'error', diagnostic: diagnosticFromError(result.error, 'PROJECT_AVAILABLE_ACTIONS_LOAD_FAILED', '无法确认你在此项目的权限。') }
      return
    }
    const data = asAvailableActions(result.data, id)
    state.value = data
      ? { kind: 'success', data }
      : { kind: 'error', diagnostic: makeDiagnostic('PROJECT_AVAILABLE_ACTIONS_INVALID', '平台返回的项目权限无法识别。', true) }
  }

  const current = computed<AvailableObjectActionsSchema | null>(() => {
    const value = state.value
    if (value.kind !== 'success' || value.data.projectId !== currentId()) return null
    return Date.parse(value.data.validUntil) > now.value ? value.data : null
  })

  function allows(action: ObjectAction): boolean {
    return current.value?.actions.includes(action) ?? false
  }

  function denialReason(action: ObjectAction, purpose?: string): string | null {
    if (allows(action)) return null
    const label = OBJECT_ACTION_LABELS[action]
    const task = purpose ?? label
    const value = state.value
    if (!currentId()) return `尚未选择项目，不能${task}。`
    if (value.kind === 'error') return `无法确认你在此项目的权限，暂不能${task}：${value.diagnostic.message}`
    if (value.kind !== 'success' || value.data.projectId !== currentId()) return `正在确认你在此项目的权限，确认前不能${task}。`
    if (!current.value) return `项目权限已过期，正在重新确认，确认前不能${task}。`
    const needs = purpose ? `，不能${purpose}` : ''
    // Archive belongs to the owner responsibility alone, which membership management cannot grant.
    if (action === 'archive') return `你在此项目的职责不包含“${label}”${needs}。归档需要项目负责人职责，不能通过成员管理授予。`
    return `你在此项目的职责不包含“${label}”${needs}。如需此权限，请联系项目负责人调整你的成员职责。`
  }

  function armExpiryTimer() {
    clearTimeout(expiryTimer)
    const value = state.value
    if (value.kind !== 'success' || !pageVisible()) return
    const delay = Math.min(
      Math.max(Date.parse(value.data.validUntil) - Date.now() + EXPIRY_SKEW_MS, MIN_REFRESH_DELAY_MS),
      MAX_TIMER_DELAY_MS,
    )
    expiryTimer = setTimeout(() => {
      now.value = Date.now()
      void load(true)
    }, delay)
  }

  function onVisibilityChange() {
    if (!pageVisible()) {
      clearTimeout(expiryTimer)
      return
    }
    now.value = Date.now()
    void load(true)
  }

  watch(state, armExpiryTimer)
  watch(projectId, () => {
    void load()
  }, { immediate: true })
  if (typeof document !== 'undefined') document.addEventListener('visibilitychange', onVisibilityChange)

  if (getCurrentScope()) {
    onScopeDispose(() => {
      loadGeneration += 1
      clearTimeout(expiryTimer)
      if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', onVisibilityChange)
    })
  }

  return {
    state,
    current,
    allows,
    denialReason,
    load,
    refresh: () => load(true),
  }
}

export type { ObjectAction, AvailableObjectActionsSchema }
