import { reactive, ref, type Ref } from 'vue'
import type { EnvironmentDetailSchema, EnvironmentOperationSnapshotSchema, ObjectAction } from '@/generated/contracts'
import type { ProjectAvailableActions } from '@/composables/useProjectAvailableActions'
import { recoverEnvironment, retryEnvironment } from '@/generated/contracts'
import { environmentOperationKindLabel, environmentStateLabel } from '@/utils/stateLabels'
import {
  STOPPED_IMPACT_NOTICE,
  failedCleanup,
  isTerminalEnvironment,
  type ConfirmedLifecycleAction,
  type EnvironmentLifecycleAction,
} from '@/utils/environmentLifecycle'
import {
  DISK_STATE_LABEL,
  RESTORE_FUNDING_NOTICE,
  diskRecoveryDiagnostic,
  environmentDiagnostic,
  failedPurge,
  retentionExpired,
} from '@/utils/environmentRetention'
import {
  createEnvironment,
  startEnvironment,
  stopEnvironment,
  restartEnvironment,
  deleteEnvironment,
} from '@/generated/contracts'
import type { CreateEnvironmentRequestSchema, EnvironmentOperationAcceptedSchema } from '@/generated/contracts'
import { extractProblemDetails, isRestartOutcomeUnknown, makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import { formatTimestamp, idempotencyKey, ifMatch } from '@/utils/format'

export type LifecycleAction = 'start' | 'stop' | 'restart' | 'delete'

const ACTION_NAME: Record<LifecycleAction, string> = { start: '启动', stop: '停止', restart: '重启', delete: '删除' }

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
          diagnostic: environmentDiagnostic(
            problem?.diagnosticCode ?? `ENVIRONMENT_${action.toUpperCase()}_FAILED`,
            problem?.detail,
            `${ACTION_NAME[action]}环境的请求没有被受理。`,
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

  /**
   * `POST /recover`: the server restores a retained disk to a new stopped
   * generation, or continues a failed purge. The caller pins `key` to one
   * intent and `revision` to the revision it confirmed, so a retry replays the
   * original receipt instead of submitting a second operation.
   */
  async function recover(environmentId: string, revision: number, key: string): Promise<LifecycleMutationResult> {
    track(`${environmentId}:recover`, true)
    try {
      const result = await recoverEnvironment({
        path: { environmentId },
        headers: { 'Idempotency-Key': key, 'If-Match': ifMatch(revision) },
      })
      if (result.error) {
        const problem = extractProblemDetails(result.error)
        return {
          ok: false,
          diagnostic: diskRecoveryDiagnostic(
            problem?.diagnosticCode ?? 'ENVIRONMENT_RECOVER_FAILED',
            problem?.detail,
            '磁盘恢复请求没有被受理。',
            problem?.retryable ?? true,
            problem,
          ),
        }
      }
      lastAccepted.value = result.data
      return { ok: true, accepted: result.data }
    } catch (error) {
      const problem = extractProblemDetails(error)
      return {
        ok: false,
        diagnostic: diskRecoveryDiagnostic(
          problem?.diagnosticCode ?? 'ENVIRONMENT_RECOVER_UNCONFIRMED',
          problem?.detail,
          '请求结果尚未确认，平台可能已经受理。',
          problem?.retryable ?? true,
          problem,
        ),
      }
    } finally {
      track(`${environmentId}:recover`, false)
    }
  }

  return reactive({ operating, lastAccepted, create, act, recover })
}

interface LifecycleTarget {
  environmentId: string
  revision: number
  action: EnvironmentLifecycleAction
}

export type DiskRecoveryIntent = 'restore' | 'purge'

interface DiskRecoveryTarget {
  environmentId: string
  revision: number
  intent: DiskRecoveryIntent
}

type InstanceState = { kind: string; data?: EnvironmentDetailSchema }

/**
 * Whether the server offers recovery for the operation the environment detail shows, as far as
 * the operation history settles it: `pending` while the history is being read or does not yet
 * hold that operation in the detail's state, `unreadable` when the history read failed, and
 * `eligible`/`ineligible` only from a snapshot of that same operation.
 */
export type OperationRecovery =
  | { kind: 'pending'; reading: boolean }
  | { kind: 'unreadable'; diagnostic: DiagnosticViewModel }
  | { kind: 'eligible' }
  | { kind: 'ineligible' }

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
  /** Recovery eligibility of the detail's current operation. */
  currentOperationRecovery: Ref<OperationRecovery>
  /** Current object actions in the environment's project. */
  permissions: ObjectActionPermissions
}

/**
 * Page-level lifecycle controls for one environment: which actions are allowed
 * and why, the stop/restart/delete confirmations, retry of the last action and
 * of a failed operation. API calls go through `useEnvironmentLifecycle`.
 */
export function useEnvironmentLifecycleControls(deps: EnvironmentLifecycleControlsDeps) {
  const { env, operations, lifecycle, activeOperation, retryableOperation, currentOperationRecovery, permissions } = deps
  const lifecycleDiagnostic = ref<DiagnosticViewModel | null>(null)
  const retryDiagnostic = ref<DiagnosticViewModel | null>(null)
  const retryingEnvironment = ref(false)
  const lastLifecycleTarget = ref<LifecycleTarget | null>(null)
  const deleteEnvironment = ref<EnvironmentDetailSchema | null>(null)
  const lifecycleConfirmation = ref<{ data: EnvironmentDetailSchema; action: ConfirmedLifecycleAction } | null>(null)
  const diskRecoveryConfirmation = ref<{ data: EnvironmentDetailSchema; intent: DiskRecoveryIntent } | null>(null)
  const diskRecoveryDiagnostic = ref<DiagnosticViewModel | null>(null)
  /** The last restore/purge continuation the server accepted on this page. */
  const diskRecoveryAccepted = ref<(DiskRecoveryTarget & { operationId: string }) | null>(null)
  const recoveringDisk = ref(false)
  const lastDiskRecovery = ref<DiskRecoveryTarget | null>(null)
  const diskRecoveryKeys = new Map<string, string>()

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

  /** A deleted environment keeps its disk for the server's fixed window; say where it stands. */
  function deletedHint(data: EnvironmentDetailSchema): string {
    const disk = data.diskRetention
    if (!disk) return '此环境已删除，没有保留的磁盘；控制台和生命周期操作均不可用。需要环境时请提交新的资源申请。'
    if (disk.state === 'retained' && !retentionExpired(disk, Date.now())) {
      return `此环境已删除，计算已停止。磁盘保留至 ${formatTimestamp(disk.expiresAt)}，可在概览的“磁盘保留与恢复”中恢复；控制台和其他生命周期操作不可用。`
    }
    return `此环境已删除，计算已停止（${DISK_STATE_LABEL[disk.state]}）；控制台和生命周期操作均不可用。`
  }

  // ---- Retained disk: restore, or continue a failed purge (both `POST /recover`) ----

  /**
   * Restore is offered only for a retained disk inside its window; purge only
   * continues one the server already started and that failed. Both are checked
   * as `edit`, like the server's recover route.
   */
  function diskRecoveryBlockReason(data: EnvironmentDetailSchema, intent: DiskRecoveryIntent, now = Date.now()): string | null {
    const denied = permissions.denialReason('edit', intent === 'restore' ? '恢复磁盘' : '继续清除磁盘')
    if (denied) return denied
    const disk = data.diskRetention
    if (!disk) return '这个环境没有保留的磁盘。'
    if (lifecycle.operating.has(`${data.id}:recover`) || recoveringDisk.value) return '请求正在提交，请等待结果。'
    if (intent === 'purge') return failedPurge(data) ? null : '只有保留期结束后未完成的清除可以继续。'
    if (data.desiredState !== 'deleted') return '环境没有被删除，不需要恢复磁盘。'
    if (hasActiveLifecycleOperation(data)) return `当前正在${environmentOperationKindLabel(activeOperation.value?.kind ?? data.operation.kind)}，请等待操作完成。`
    if (disk.state === 'retiring') return '计算尚未确认停止，确认后才能恢复磁盘。'
    if (disk.state !== 'retained') return `${DISK_STATE_LABEL[disk.state]}，不能恢复。`
    if (retentionExpired(disk, now)) return `七天保留期已于 ${formatTimestamp(disk.expiresAt)} 结束，不能恢复。`
    return null
  }

  function requestDiskRecovery(data: EnvironmentDetailSchema, intent: DiskRecoveryIntent) {
    if (diskRecoveryBlockReason(data, intent) !== null) return
    diskRecoveryConfirmation.value = { data, intent }
  }

  /**
   * One Idempotency-Key per (intent, environment, confirmed revision), kept for
   * this page: retrying after a lost answer replays the original receipt. After
   * a refresh the If-Match fence rejects a second command for a revision the
   * first one already advanced, so neither path submits a second operation.
   */
  async function submitDiskRecovery(target: DiskRecoveryTarget) {
    if (recoveringDisk.value) return
    const scope = `${target.intent}:${target.environmentId}:${target.revision}`
    const key = diskRecoveryKeys.get(scope) ?? idempotencyKey()
    diskRecoveryKeys.set(scope, key)
    lastDiskRecovery.value = target
    diskRecoveryDiagnostic.value = null
    recoveringDisk.value = true
    try {
      const result = await lifecycle.recover(target.environmentId, target.revision, key)
      if (!result.ok) {
        diskRecoveryDiagnostic.value = result.diagnostic
        refreshPermissionsAfter(result.diagnostic)
        return
      }
      diskRecoveryKeys.delete(scope)
      lastDiskRecovery.value = null
      diskRecoveryAccepted.value = { ...target, operationId: result.accepted.operationId }
    } finally {
      recoveringDisk.value = false
    }
    await Promise.all([env.load(), operations.load()])
  }

  async function confirmDiskRecovery() {
    const pending = diskRecoveryConfirmation.value
    diskRecoveryConfirmation.value = null
    if (!pending) return
    const data = currentInstance()
    if (!data || data.id !== pending.data.id || data.revision !== pending.data.revision) return
    if (diskRecoveryBlockReason(data, pending.intent) !== null) return
    await submitDiskRecovery({ environmentId: data.id, revision: data.revision, intent: pending.intent })
  }

  /** Retry resends the unconfirmed command itself: same key, same revision. */
  async function retryDiskRecovery() {
    const target = lastDiskRecovery.value
    if (!target || currentInstance()?.id !== target.environmentId) return
    if (!permissions.allows('edit')) return
    await submitDiskRecovery(target)
  }

  function lifecycleActionReason(data: EnvironmentDetailSchema, action: EnvironmentLifecycleAction): string {
    if (data.observedState === 'deleted') return deletedHint(data)
    const denied = permissions.denialReason(
      LIFECYCLE_OBJECT_ACTION[action],
      action === 'restart' ? '重启环境' : action === 'delete' ? '删除环境' : undefined,
    )
    if (denied) return denied
    if (failedCleanup(data) && !hasActiveLifecycleOperation(data)) return action === 'delete'
      ? data.diskRetention
        ? '上次回收失败，计算释放尚未确认；重新回收会继续停止计算，工作盘仍按原保留期保留。'
        : '上次回收失败，资源释放尚未确认；重新回收会删除仍存在的工作目录和关联磁盘数据。'
      : '环境正在等待回收，只能重试回收，不能重新启动或恢复环境。'
    if (failedPurge(data)) return '磁盘清除没有完成，只能在“磁盘保留与恢复”中继续清除。'
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
    if (data.observedState === 'deleted') return deletedHint(data)
    if (failedCleanup(data) && !hasActiveLifecycleOperation(data)) {
      return data.diskRetention
        ? '环境回收失败，计算释放尚未确认；工作盘仍按原保留期保留。请重试回收；回收完成前不能打开控制台或重新启动环境。'
        : '环境回收失败，资源释放尚未确认。请重试回收；回收完成前不能打开控制台或重新启动环境。'
    }
    if (failedPurge(data)) return '计算已停止，磁盘清除没有完成。请在“磁盘保留与恢复”中查看原因并继续清除。'
    if (data.observedState === 'deleting' || data.desiredState === 'deleted') return '删除已请求/正在回收，控制台和其他生命周期操作会保持禁用，直到清理完成。'
    if (hasActiveLifecycleOperation(data)) return `当前正在${environmentOperationKindLabel(activeOperation.value?.kind ?? data.operation.kind)}，请在操作完成后继续。`
    if (data.observedState === 'ready') return '环境已就绪，可以打开终端；重启会中断当前运行。'
    if (data.observedState === 'stopped' && data.diskRetention?.state === 'restored' && !data.diskRetention.reactivatedAt) {
      return `磁盘已恢复到第 ${data.diskRetention.restoredGeneration ?? data.generation} 代，环境已停止。${RESTORE_FUNDING_NOTICE}如果在 ${formatTimestamp(data.diskRetention.expiresAt)} 前没有重新运行，平台仍会按原期限清除磁盘。`
    }
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
   * retried by the page; the next step names the toolbar recovery once the server's answer for
   * that operation is read, and says none is available only when the server declined it.
   */
  function failedEnvironmentDiagnostic(data: EnvironmentDetailSchema): DiagnosticViewModel {
    if (failedCleanup(data)) {
      return data.diskRetention
        ? { code: '', message: '环境回收失败，计算释放尚未确认；工作盘按原保留期保留。请使用顶部操作栏的“重试回收”。', retryable: false }
        : { code: '', message: '环境回收失败，剩余工作目录和关联磁盘数据可能仍存在，资源释放尚未确认。请使用顶部操作栏的“重试回收”。', retryable: false }
    }
    if (failedPurge(data)) {
      return { code: data.operation.diagnosticCode ?? '', message: '保留期已结束，但磁盘清除没有完成，磁盘可能仍然存在。请在概览的“磁盘保留与恢复”中继续清除。', retryable: false }
    }
    const code = data.operation.state === 'failed' ? data.operation.diagnosticCode : null
    if (isRestartOutcomeUnknown(code)) {
      const diagnostic = makeDiagnostic(code, '', false)
      const recovery = currentOperationRecovery.value
      if (recovery.kind === 'eligible') return diagnostic
      if (recovery.kind === 'ineligible') {
        return { ...diagnostic, nextStep: '平台不会自动重试。请在“异步操作与诊断”中核对环境当前状态；平台对这次操作没有提供恢复，请把下方诊断信息发给管理员。' }
      }
      if (recovery.kind === 'unreadable') {
        return { ...diagnostic, nextStep: `平台不会自动重试。操作历史读取失败（${recovery.diagnostic.message}），暂时无法确认能否恢复；请重新读取操作历史。` }
      }
      return {
        ...diagnostic,
        nextStep: recovery.reading
          ? '平台不会自动重试。正在读取操作历史以确认能否恢复，确认前不要重复提交重启。'
          : '平台不会自动重试。操作历史还没有这次操作的最新结果，暂时无法确认能否恢复；请重新读取操作历史。',
      }
    }
    const phase = data.failedPhase ? `（${environmentStateLabel(data.failedPhase)}阶段）` : ''
    return { code: '', message: `环境${phase}未能完成操作。${retryableOperation.value ? '请使用顶部操作栏的“重试失败的操作”。' : ''}`, retryable: false }
  }

  /** Offers to re-read the operation history when an unknown restart's recovery is still unsettled and nothing is reading it. */
  function canRereadRecovery(data: EnvironmentDetailSchema): boolean {
    const recovery = currentOperationRecovery.value
    return data.operation.state === 'failed'
      && isRestartOutcomeUnknown(data.operation.diagnosticCode)
      && !failedCleanup(data)
      && (recovery.kind === 'unreadable' || (recovery.kind === 'pending' && !recovery.reading))
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
    diskRecoveryDiagnostic.value = null
    diskRecoveryAccepted.value = null
    lastDiskRecovery.value = null
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
    canRereadRecovery,
    runLifecycle,
    requestLifecycleConfirmation,
    confirmLifecycle,
    retryFailedOperation,
    retryLifecycle,
    openDelete,
    confirmDeleteEnvironment,
    resetDiagnostics,
    diskRecoveryConfirmation,
    diskRecoveryDiagnostic,
    diskRecoveryAccepted,
    recoveringDisk,
    lastDiskRecovery,
    diskRecoveryBlockReason,
    requestDiskRecovery,
    confirmDiskRecovery,
    retryDiskRecovery,
  }
}
