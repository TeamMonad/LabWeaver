import { reactive, ref } from 'vue'
import { describe, expect, it, vi } from 'vitest'
import type { EnvironmentInstanceSchema, EnvironmentOperationSnapshotSchema } from '@/generated/contracts'
import { useEnvironmentLifecycleControls } from '@/composables/useEnvironmentLifecycle'
import {
  accessGrantBlockReason,
  canIssueAccessGrant,
  deleteDialogCopy,
  failedCleanup,
  lifecycleConfirmationText,
} from '@/utils/environmentLifecycle'

function instance(patch: Partial<EnvironmentInstanceSchema> = {}): EnvironmentInstanceSchema {
  return {
    id: 'env-1',
    displayLabel: '光学实验',
    revision: 3,
    desiredState: 'running',
    observedState: 'ready',
    operation: { kind: 'start', state: 'succeeded' },
    ...patch,
  } as unknown as EnvironmentInstanceSchema
}

function controls(data: EnvironmentInstanceSchema, act = vi.fn(async () => ({ ok: true as const, accepted: {} }))) {
  const env = reactive({ instance: { kind: 'success', data }, load: vi.fn(async () => undefined) })
  const operations = { load: vi.fn(async () => undefined) }
  const lifecycle = reactive({ operating: new Set<string>(), lastAccepted: null, create: vi.fn(), act })
  return {
    act,
    env,
    controls: useEnvironmentLifecycleControls({
      env,
      operations,
      lifecycle: lifecycle as never,
      activeOperation: ref<EnvironmentOperationSnapshotSchema | null>(null),
      retryableOperation: ref<EnvironmentOperationSnapshotSchema | null>(null),
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

  it('only issues access grants for a running, ready environment', () => {
    expect(canIssueAccessGrant(instance())).toBe(true)
    const stopped = instance({ desiredState: 'stopped', observedState: 'stopped' })
    expect(canIssueAccessGrant(stopped)).toBe(false)
    expect(accessGrantBlockReason(stopped)).toBe('环境已停止，启动后才能签发访问授权。')
  })
})
