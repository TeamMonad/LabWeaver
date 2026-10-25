import { reactive, ref, type Ref } from 'vue'
import type { EnvironmentDetailSchema, EnvironmentOperationSnapshotSchema, ObjectAction } from '@/generated/contracts'
import type { ProjectAvailableActions } from '@/composables/useProjectAvailableActions'
import { retryEnvironment } from '@/generated/contracts'
import { environmentOperationKindLabel, environmentStateLabel } from '@/utils/stateLabels'
import {
  STOPPED_IMPACT_NOTICE,
  failedCleanup,
  isTerminalEnvironment,
  type ConfirmedLifecycleAction,
  type EnvironmentLifecycleAction,
} from '@/utils/environmentLifecycle'
import {
  createEnvironment,
  startEnvironment,
  stopEnvironment,
  restartEnvironment,
  deleteEnvironment,
} from '@/generated/contracts'
import type { CreateEnvironmentRequestSchema, EnvironmentOperationAcceptedSchema } from '@/generated/contracts'
import { extractProblemDetails, isRestartOutcomeUnknown, makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

export type LifecycleAction = 'start' | 'stop' | 'restart' | 'delete'

/**
 * Public create only attaches to the environment that Resource's approved
 * request already handed off, so it always names that real request. The
 * request's own course scope is sent as-is.
 */
type CreateEnvironmentInput = Pick<CreateEnvironmentRequestSchema, 'resourceRequestId' | 'releaseId' | 'releaseVersion'> &
  Partial<Pick<CreateEnvironmentRequestSchema, 'courseId' | 'displayLabel'>>

export type LifecycleMutationResult =
  | { ok: true; accepted: EnvironmentOperationAcceptedSchema }
  | { ok: false; diagnostic: DiagnosticViewModel }

export function useEnvironmentLifecycle(
  projectId: Ref<string | undefined>,
  courseId: Ref<string | undefined> = ref(undefined),
) {
  const operating = ref<Set<string>>(new Set())
  const lastAccepted = ref<EnvironmentOperationAcceptedSchema | null>(null)

  function track(id: string, active: boolean) {
    if (active) operating.value.add(id)
    else operating.value.delete(id)
  }

  /**
   * `intentKey` lets the caller pin one Idempotency-Key to one logical create
   * intent so that retrying after a network timeout replays the same create
   * instead of minting a second environment. Omit it only for first attempts.
   */
  async function create(request: CreateEnvironmentInput, intentKey?: string): Promise<LifecycleMutationResult> {
    const id = projectId.value
    if (!id) {
      return {
        ok: false,
        diagnostic: makeDiagnostic('PROJECT_CONTEXT_MISSING', '项目上下文缺失，无法创建环境。', false),
      }
    }
    let result
    try {
      result = await createEnvironment({
        headers: { 'Idempotency-Key': intentKey ?? idempotencyKey() },
        body: {
          projectId: id,
          ...(courseId.value ? { courseId: courseId.value } : {}),
          ...request,
        },
      })
    } catch (error) {
      const problem = extractProblemDetails(error)
      return {
        ok: false,
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'ENVIRONMENT_CREATE_FAILED',
          problem?.detail ?? '创建环境失败，结果可能尚未确认。',
          problem?.retryable ?? true,
          problem,
        ),
      }
    }
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      return {
        ok: false,
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'ENVIRONMENT_CREATE_FAILED',
          problem?.detail ?? '创建环境失败',
          problem?.retryable ?? true,
          problem,
        ),
      }
    }
    lastAccepted.value = result.data
    return { ok: true, accepted: result.data }
  }

  async function act(environmentId: string, revision: number, action: LifecycleAction): Promise<LifecycleMutationResult> {
    track(`${environmentId}:${action}`, true)
    try {
      const headers = { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(revision) }
      const path = { environmentId }
      const fn =
        action === 'start'
          ? startEnvironment({ path, headers })
          : action === 'stop'
            ? stopEnvironment({ path, headers })
            : action === 'restart'
              ? restartEnvironment({ path, headers })
              : deleteEnvironment({ path, headers })
      const result = await fn
      if (result.error) {
        const problem = extractProblemDetails(result.error)
        return {
          ok: false,
          diagnostic: makeDiagnostic(
            problem?.diagnosticCode ?? `ENVIRONMENT_${action.toUpperCase()}_FAILED`,
            problem?.detail ?? `${action} 失败`,
            problem?.retryable ?? true,
            problem,
          ),
        }
      }
      lastAccepted.value = result.data
      return { ok: true, accepted: result.data }
    } finally {
      track(`${environmentId}:${action}`, false)
    }
  }

  return reactive({ operating, lastAccepted, create, act })
}

interface LifecycleTarget {
  environmentId: string
  revision: number
  action: EnvironmentLifecycleAction
}

type InstanceState = { kind: string; data?: EnvironmentDetailSchema }

/** The part of `useProjectAvailableActions` that page controls consume. */
export type ObjectActionPermissions = Pick<ProjectAvailableActions, 'current' | 'allows' | 'denialReason' | 'refresh'>

const LIFECYCLE_PERMISSION_NAMES = { start: '启动和重启', stop: '停止和取消', edit: '删除和重试' } as const

/** Object action the server checks for each lifecycle route (contracts `environment_operation_action`). */
export const LIFECYCLE_OBJECT_ACTION: Record<EnvironmentLifecycleAction, ObjectAction> = {
  start: 'start',
  restart: 'start',
  stop: 'stop',
  delete: 'edit',
}

export interface EnvironmentLifecycleControlsDeps {
  /** Current environment instance (the `useEnvironmentInstance` reactive). */
  env: { instance: InstanceState; load: () => Promise<unknown> }
  operations: { load: () => Promise<unknown> }
  lifecycle: ReturnType<typeof useEnvironmentLifecycle>
  activeOperation: Ref<EnvironmentOperationSnapshotSchema | null>
  retryableOperation: Ref<EnvironmentOperationSnapshotSchema | null>
  /** Current object actions in the environment's project. */
  permissions: ObjectActionPermissions
}

/**
 * Page-level lifecycle controls for one environment: which actions are allowed
 * and why, the stop/restart/delete confirmations, retry of the last action and
 * of a failed operation. API calls go through `useEnvironmentLifecycle`.
 */
export function useEnvironmentLifecycleControls(deps: EnvironmentLifecycleControlsDeps) {
  const { env, operations, lifecycle, activeOperation, retryableOperation, permissions } = deps
  const lifecycleDiagnostic = ref<DiagnosticViewModel | null>(null)
  const retryDiagnostic = ref<DiagnosticViewModel | null>(null)
  const retryingEnvironment = ref(false)
  const lastLifecycleTarget = ref<LifecycleTarget | null>(null)
  const deleteEnvironment = ref<EnvironmentDetailSchema | null>(null)
  const lifecycleConfirmation = ref<{ data: EnvironmentDetailSchema; action: ConfirmedLifecycleAction } | null>(null)

  function currentInstance(): EnvironmentDetailSchema | undefined {
    return env.instance.kind === 'success' ? env.instance.data : undefined
  }

  function hasActiveLifecycleOperation(data: EnvironmentDetailSchema): boolean {
    const current = data.operation.state
    if (current === 'accepted' || current === 'running' || current === 'cancelling') return true
    const accepted = lifecycle.lastAccepted
    if (accepted?.environmentId === data.id && accepted.revision > data.revision) return true
    return activeOperation.value?.environmentId === data.id
  }

  function canStart(data: EnvironmentDetailSchema) {
    return permissions.allows('start')
      && !isTerminalEnvironment(data)
      && !hasActiveLifecycleOperation(data)
      && data.desiredState !== 'deleted'
      && data.desiredState !== 'running'
      && data.observedState === 'stopped'
      && !lifecycle.operating.has(`${data.id}:start`)
  }

  function canStop(data: EnvironmentDetailSchema) {
    return permissions.allows('stop')
      && !isTerminalEnvironment(data)
      && !hasActiveLifecycleOperation(data)
      && data.desiredState !== 'deleted'
      && data.desiredState !== 'stopped'
      && data.observedState === 'ready'
      && !lifecycle.operating.has(`${data.id}:stop`)
  }

  function canRestart(data: EnvironmentDetailSchema) {
    return permissions.allows('start')
      && !isTerminalEnvironment(data)
      && !hasActiveLifecycleOperation(data)
      && data.desiredState !== 'deleted'
      && (data.observedState === 'ready' || data.observedState === 'stopped' || data.observedState === 'failed')
      && !lifecycle.operating.has(`${data.id}:restart`)
  }

  function canDelete(data: EnvironmentDetailSchema) {
    return permissions.allows('edit')
      && !isTerminalEnvironment(data)
      && (data.desiredState !== 'deleted' || (failedCleanup(data) && !hasActiveLifecycleOperation(data)))
      && !(hasActiveLifecycleOperation(data) && data.operation.kind === 'delete')
      && !lifecycle.operating.has(`${data.id}:delete`)
  }

  /** Retrying a failed operation is checked as `edit`, like reset and recover. */
  function canRetryFailedOperation() {
    return permissions.allows('edit')
  }

  function lifecycleActionReason(data: EnvironmentDetailSchema, action: EnvironmentLifecycleAction): string {
    if (data.observedState === 'deleted') return '此项目环境已删除，不能再执行生命周期操作。请返回项目环境列表创建新的环境。'
    const denied = permissions.denialReason(
      LIFECYCLE_OBJECT_ACTION[action],
      action === 'restart' ? '重启环境' : action === 'delete' ? '删除环境' : undefined,
    )
    if (denied) return denied
    if (failedCleanup(data) && !hasActiveLifecycleOperation(data)) return action === 'delete'
      ? '上次回收失败，资源释放尚未确认；重新回收会删除仍存在的工作目录和关联磁盘数据。'
      : '环境正在等待回收，只能重试回收，不能重新启动或恢复环境。'
    if (data.observedState === 'deleting' || data.desiredState === 'deleted') return '删除已请求/正在回收，请等待清理完成。'
    if (hasActiveLifecycleOperation(data) && (action !== 'delete' || data.operation.kind === 'delete')) {
      return `当前正在${environmentOperationKindLabel(activeOperation.value?.kind ?? data.operation.kind)}，请等待操作完成。`
    }
    if (action === 'start' && data.desiredState === 'running') return '环境已经是运行目标状态。'
    if (action === 'start' && data.observedState !== 'stopped') return `环境当前为${environmentStateLabel(data.observedState)}，只能在已停止状态启动。`
    if (action === 'stop' && data.desiredState === 'stopped') return '环境已经是停止目标状态。'
    if (action === 'stop' && data.observedState !== 'ready') return `环境当前为${environmentStateLabel(data.observedState)}，就绪后才能停止。`
    if (action === 'restart' && !['ready', 'stopped', 'failed'].includes(data.observedState)) return '只有就绪、已停止或失败的环境才能重启。'
    return `${action === 'start' ? '启动' : action === 'stop' ? '停止' : action === 'restart' ? '重启' : '删除'}环境`
  }

  function lifecycleActionHint(data: EnvironmentDetailSchema): string {
    if (data.observedState === 'deleted') return '此项目环境已删除，控制台和生命周期操作均不可用。请返回项目环境列表创建新的环境。'
    if (failedCleanup(data) && !hasActiveLifecycleOperation(data)) return '环境回收失败，资源释放尚未确认。请重试回收；回收完成前不能打开控制台或重新启动环境。'
    if (data.observedState === 'deleting' || data.desiredState === 'deleted') return '删除已请求/正在回收，控制台和其他生命周期操作会保持禁用，直到清理完成。'
    if (hasActiveLifecycleOperation(data)) return `当前正在${environmentOperationKindLabel(activeOperation.value?.kind ?? data.operation.kind)}，请在操作完成后继续。`
    if (data.observedState === 'ready') return '环境已就绪，可以打开终端；重启会中断当前运行。'
    if (data.observedState === 'stopped') return `环境已停止，启动后才能打开终端。工作目录和磁盘仍保留。${STOPPED_IMPACT_NOTICE}`
    if (data.observedState === 'failed') {
      if (data.operation.state === 'failed' && isRestartOutcomeUnknown(data.operation.diagnosticCode)) {
        return '上次重启结果未知，平台不会自动重试。请先在“异步操作与诊断”中核对环境状态，再手动选择恢复。'
      }
      return retryableOperation.value ? '上次操作失败，可以重试失败的操作。' : '上次操作失败，请先查看操作诊断。'
    }
    return `环境当前为${environmentStateLabel(data.observedState)}，请等待状态更新。`
  }

  /**
   * Which lifecycle actions the actor's project responsibility leaves out, in
   * one sentence; null when every lifecycle action is permitted. Shown next to
   * the action bar so a disabled button never needs a hover to be explained.
   */
  function lifecyclePermissionHint(): string | null {
    const missing = (['start', 'stop', 'edit'] as const).filter((action) => !permissions.allows(action))
    if (missing.length === 0) return null
    // Until the project's actions are confirmed, the reason is the same for every action.
    if (!permissions.current.value) return permissions.denialReason(missing[0])
    const names = missing.map((action) => LIFECYCLE_PERMISSION_NAMES[action]).join('、')
    return `你在此项目的职责不包含${names}环境。如需此权限，请联系项目负责人调整你的成员职责。`
  }

  /** A 403 means the actor's project responsibility changed since the page read it. */
  function refreshPermissionsAfter(diagnostic: DiagnosticViewModel | null | undefined) {
    if (diagnostic?.status === 403) void permissions.refresh()
  }

  /**
   * Why the environment is failed and what the user can do. An unknown restart outcome is never
   * retried by the page; the next step names the toolbar recovery only while the server offers it.
   */
  function failedEnvironmentDiagnostic(data: EnvironmentDetailSchema): DiagnosticViewModel {
    if (failedCleanup(data)) {
      return { code: '', message: '环境回收失败，剩余工作目录和关联磁盘数据可能仍存在，资源释放尚未确认。请使用顶部操作栏的“重试回收”。', retryable: false }
    }
    const code = data.operation.state === 'failed' ? data.operation.diagnosticCode : null
    if (isRestartOutcomeUnknown(code)) {
      const diagnostic = makeDiagnostic(code, '', false)
      if (retryableOperation.value) return diagnostic
      return { ...diagnostic, nextStep: '平台不会自动重试。请在“异步操作与诊断”中核对环境当前状态；当前没有可用的恢复操作，请把下方诊断信息发给管理员。' }
    }
    const phase = data.failedPhase ? `（${environmentStateLabel(data.failedPhase)}阶段）` : ''
    return { code: '', message: `环境${phase}未能完成操作。${retryableOperation.value ? '请使用顶部操作栏的“重试失败的操作”。' : ''}`, retryable: false }
  }

  async function runLifecycle(data: EnvironmentDetailSchema, action: EnvironmentLifecycleAction) {
    lifecycleDiagnostic.value = null
    lastLifecycleTarget.value = { environmentId: data.id, revision: data.revision, action }
    const result = await lifecycle.act(data.id, data.revision, action)
    if (!result.ok) {
      lifecycleDiagnostic.value = result.diagnostic ?? null
      refreshPermissionsAfter(result.diagnostic)
      return
    }
    // Refresh the operations timeline immediately so the new command shows up
    // instead of waiting for the next full page load.
    await Promise.all([env.load(), operations.load()])
  }

  /** Stop and restart interrupt sessions and change usage, so they confirm first; start never does. */
  function requestLifecycleConfirmation(data: EnvironmentDetailSchema, action: ConfirmedLifecycleAction) {
    lifecycleConfirmation.value = { data, action }
  }

  async function confirmLifecycle() {
    const pending = lifecycleConfirmation.value
    lifecycleConfirmation.value = null
    if (pending) await runLifecycle(pending.data, pending.action)
  }

  async function retryFailedOperation(data: EnvironmentDetailSchema) {
    if (!canRetryFailedOperation()) return
    retryDiagnostic.value = null
    retryingEnvironment.value = true
    try {
      const result = await retryEnvironment({
        path: { environmentId: data.id },
        headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(data.revision) },
      })
      if (result.error) {
        const problem = extractProblemDetails(result.error)
        retryDiagnostic.value = makeDiagnostic(
          problem?.diagnosticCode ?? 'ENVIRONMENT_RETRY_FAILED',
          problem?.detail ?? '重试失败的操作未成功',
          problem?.retryable ?? true,
          problem,
        )
        refreshPermissionsAfter(retryDiagnostic.value)
        return
      }
      await Promise.all([env.load(), operations.load()])
    } finally {
      retryingEnvironment.value = false
    }
  }

  async function retryLifecycle() {
    const target = lastLifecycleTarget.value
    if (!target) return
    const instance = currentInstance()
    if (!instance || instance.id !== target.environmentId) return
    if (!permissions.allows(LIFECYCLE_OBJECT_ACTION[target.action])) return
    if (target.action === 'delete' && !canDelete(instance)) return
    await runLifecycle(instance, target.action)
  }

  function openDelete(data: EnvironmentDetailSchema) {
    if (!canDelete(data)) return
    deleteEnvironment.value = data
  }

  async function confirmDeleteEnvironment() {
    if (!deleteEnvironment.value) return
    const requested = deleteEnvironment.value
    deleteEnvironment.value = null
    const data = currentInstance()
    if (!data || data.id !== requested.id || !canDelete(data)) return
    await runLifecycle(data, 'delete')
  }

  function resetDiagnostics() {
    lifecycleDiagnostic.value = null
    retryDiagnostic.value = null
  }

  return {
    lifecycleDiagnostic,
    retryDiagnostic,
    retryingEnvironment,
    deleteEnvironment,
    lifecycleConfirmation,
    hasActiveLifecycleOperation,
    canStart,
    canStop,
    canRestart,
    canDelete,
    canRetryFailedOperation,
    lifecycleActionReason,
    lifecycleActionHint,
    lifecyclePermissionHint,
    failedEnvironmentDiagnostic,
    runLifecycle,
    requestLifecycleConfirmation,
    confirmLifecycle,
    retryFailedOperation,
    retryLifecycle,
    openDelete,
    confirmDeleteEnvironment,
    resetDiagnostics,
  }
}
