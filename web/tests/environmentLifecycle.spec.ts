import { computed, reactive, ref } from 'vue'
import { describe, expect, it, vi } from 'vitest'
import type { EnvironmentDetailSchema, EnvironmentOperationSnapshotSchema, ObjectAction } from '@/generated/contracts'
import { useEnvironmentLifecycleControls, type ObjectActionPermissions } from '@/composables/useEnvironmentLifecycle'
import {
  accessGrantBlockReason,
  canIssueAccessGrant,
  deleteDialogCopy,
  failedCleanup,
  lifecycleConfirmationText,
} from '@/utils/environmentLifecycle'

function instance(patch: Partial<EnvironmentDetailSchema> = {}): EnvironmentDetailSchema {
  return {
    id: 'env-1',
    displayLabel: '光学实验',
    revision: 3,
    desiredState: 'running',
    observedState: 'ready',
    operation: { kind: 'start', state: 'succeeded' },
    ...patch,
  } as unknown as EnvironmentDetailSchema
}

const ALL_ACTIONS: ObjectAction[] = ['browse', 'edit', 'start', 'stop', 'configure_agent', 'grant_access', 'manage_members', 'view_costs']

/** The project's current object actions, as `useProjectAvailableActions` would expose them after a successful read. */
function permissionsFor(actions: ObjectAction[]): ObjectActionPermissions {
  const current = computed(() => ({ projectId: 'project-1', actorId: 'actor-1', membershipRevision: 1, validUntil: '2099-01-01T00:00:00.000Z', actions }))
  return {
    current,
    allows: (action) => actions.includes(action),
    denialReason: (action, purpose) => (actions.includes(action) ? null : `职责不包含 ${action}${purpose ? `，不能${purpose}` : ''}`),
    refresh: vi.fn(async () => undefined),
  }
}

function controls(
  data: EnvironmentDetailSchema,
  act = vi.fn(async () => ({ ok: true as const, accepted: {} })),
  permissions = permissionsFor(ALL_ACTIONS),
) {
  const env = reactive({ instance: { kind: 'success', data }, load: vi.fn(async () => undefined) })
  const operations = { load: vi.fn(async () => undefined) }
  const lifecycle = reactive({ operating: new Set<string>(), lastAccepted: null, create: vi.fn(), act })
  return {
    act,
    env,
    permissions,
    controls: useEnvironmentLifecycleControls({
      env,
      operations,
      lifecycle: lifecycle as never,
      activeOperation: ref<EnvironmentOperationSnapshotSchema | null>(null),
      retryableOperation: ref<EnvironmentOperationSnapshotSchema | null>(null),
      permissions,
    }),
  }
}

describe('environment lifecycle rules', () => {
  it('allows stop/restart on a ready environment and start only when stopped', () => {
    const { controls: ready } = controls(instance())
    expect(ready.canStop(instance())).toBe(true)
    expect(ready.canRestart(instance())).toBe(true)
    expect(ready.canStart(instance())).toBe(false)
    expect(ready.lifecycleActionReason(instance(), 'start')).toBe('环境已经是运行目标状态。')
    const stopped = instance({ desiredState: 'stopped', observedState: 'stopped' })
    expect(ready.canStart(stopped)).toBe(true)
    expect(ready.lifecycleActionHint(stopped)).toContain('GPU 容量是否保留取决于环境类型和项目资源配置')
  })

  it('blocks every action while an operation is running', () => {
    const busy = instance({ operation: { kind: 'restart', state: 'running' } as never })
    const { controls: c } = controls(busy)
    expect([c.canStart(busy), c.canStop(busy), c.canRestart(busy)]).toEqual([false, false, false])
    expect(c.lifecycleActionHint(busy)).toContain('请在操作完成后继续')
  })

  it('treats a failed cleanup as retry-only and words the delete dialog for it', () => {
    const failed = instance({ desiredState: 'deleted', observedState: 'failed', operation: { kind: 'delete', state: 'failed' } as never })
    expect(failedCleanup(failed)).toBe(true)
    const { controls: c } = controls(failed)
    expect(c.canDelete(failed)).toBe(true)
    expect(c.canStart(failed)).toBe(false)
    expect(deleteDialogCopy(failed).confirmText).toBe('重试回收')
    expect(deleteDialogCopy(instance()).title).toBe('删除环境')
  })

  it('confirms stop with neutral impact wording before acting', async () => {
    const data = instance()
    const { controls: c, act } = controls(data)
    c.requestLifecycleConfirmation(data, 'stop')
    expect(act).not.toHaveBeenCalled()
    expect(lifecycleConfirmationText(data, 'stop')).not.toMatch(/GPU 预留(已|会)?释放/)
    await c.confirmLifecycle()
    expect(act).toHaveBeenCalledWith('env-1', 3, 'stop')
    expect(c.lifecycleConfirmation.value).toBeNull()
  })

  it('keeps the failed action diagnostic and retries the same action', async () => {
    const data = instance()
    const act = vi.fn()
      .mockResolvedValueOnce({ ok: false, diagnostic: { code: 'LW_X', message: 'x', retryable: true } })
      .mockResolvedValueOnce({ ok: true, accepted: {} })
    const { controls: c } = controls(data, act as never)
    await c.runLifecycle(data, 'restart')
    expect(c.lifecycleDiagnostic.value?.code).toBe('LW_X')
    await c.retryLifecycle()
    expect(act).toHaveBeenLastCalledWith('env-1', 3, 'restart')
    expect(c.lifecycleDiagnostic.value).toBeNull()
  })

  it('only issues access grants once the server reports the running instance ready', () => {
    const connection = { instanceReady: true, state: 'preparing' as const, guestInstructions: [] as string[] }
    expect(canIssueAccessGrant({ desiredState: 'running', connection })).toBe(true)
    // An observed `ready` is not enough: readiness comes from the connection read model.
    expect(canIssueAccessGrant({ desiredState: 'running', connection: { ...connection, instanceReady: false } })).toBe(false)
    expect(canIssueAccessGrant({ desiredState: 'stopped', connection })).toBe(false)
    const stopped = instance({ desiredState: 'stopped', observedState: 'stopped' })
    expect(accessGrantBlockReason(stopped)).toBe('环境已停止，启动后才能签发访问授权。')
  })

  it('offers only the lifecycle actions the project responsibility contains and says why the rest are missing', () => {
    const ready = instance()
    const stopped = instance({ desiredState: 'stopped', observedState: 'stopped' })
    const { controls: operator } = controls(ready, undefined, permissionsFor(['browse', 'start', 'stop']))
    expect(operator.canStop(ready)).toBe(true)
    expect(operator.canRestart(ready)).toBe(true)
    expect(operator.canStart(stopped)).toBe(true)
    expect(operator.canDelete(ready)).toBe(false)
    expect(operator.canRetryFailedOperation()).toBe(false)
    // The reason for a missing action is the permission reason, and the hint names only what is missing.
    expect(operator.lifecycleActionReason(ready, 'delete')).toContain('edit')
    expect(operator.lifecyclePermissionHint()).toContain('删除')
    expect(operator.lifecyclePermissionHint()).not.toContain('启动')

    const { controls: viewer } = controls(ready, undefined, permissionsFor(['browse']))
    expect([viewer.canStart(stopped), viewer.canStop(ready), viewer.canRestart(ready), viewer.canDelete(ready)]).toEqual([false, false, false, false])
    // The permission reason comes before the state reason, so a viewer is not told to wait for a state.
    expect(viewer.lifecycleActionReason(stopped, 'start')).toContain('start')
    expect(viewer.lifecycleActionReason(ready, 'restart')).toContain('start')
    for (const name of ['启动', '停止', '删除']) expect(viewer.lifecyclePermissionHint()).toContain(name)

    const { controls: editor } = controls(ready)
    expect(editor.lifecyclePermissionHint()).toBeNull()
  })

  it('does not send a lifecycle request the project responsibility lacks, and re-reads permissions after a 403', async () => {
    const data = instance()
    const act = vi.fn()
      .mockResolvedValueOnce({ ok: false, diagnostic: { code: 'LW_AUTH_SCOPE_DENIED', message: 'denied', retryable: false, status: 403 } })
    const permissions = permissionsFor(ALL_ACTIONS)
    const { controls: c } = controls(data, act as never, permissions)
    await c.runLifecycle(data, 'stop')
    expect(c.lifecycleDiagnostic.value?.code).toBe('LW_AUTH_SCOPE_DENIED')
    expect(permissions.refresh).toHaveBeenCalled()

    const refused = vi.fn().mockResolvedValueOnce({ ok: false, diagnostic: { code: 'LW_X', message: 'x', retryable: true, status: 409 } })
    const other = permissionsFor(ALL_ACTIONS)
    await controls(data, refused as never, other).controls.runLifecycle(data, 'stop')
    // Only a refusal of authority re-reads permissions.
    expect(other.refresh).not.toHaveBeenCalled()

    const viewer = controls(data, vi.fn() as never, permissionsFor(['browse']))
    viewer.controls.openDelete(data)
    expect(viewer.controls.deleteEnvironment.value).toBeNull()
    await viewer.controls.retryFailedOperation(data)
    expect(viewer.controls.retryingEnvironment.value).toBe(false)
    expect(viewer.act).not.toHaveBeenCalled()
  })
})
