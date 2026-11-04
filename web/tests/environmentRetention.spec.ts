import { computed, defineComponent, h, reactive, ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import type { EnvironmentDetailSchema, EnvironmentOperationSnapshotSchema, ObjectAction } from '@/generated/contracts'
import { recoverEnvironment, startEnvironment } from '@/generated/contracts'
import {
  useEnvironmentLifecycle,
  useEnvironmentLifecycleControls,
  type ObjectActionPermissions,
  type OperationRecovery,
} from '@/composables/useEnvironmentLifecycle'
import { provideEnvironmentEntry, type EnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import EnvironmentDiskRetentionPanel from '@/components/environment/EnvironmentDiskRetentionPanel.vue'
import EnvironmentRuntimePermissions from '@/components/environment/EnvironmentRuntimePermissions.vue'
import { deleteDialogCopy } from '@/utils/environmentLifecycle'
import {
  computeStatusText,
  deadlineText,
  effectivePermissionRows,
  environmentDiagnostic,
  restoreConfirmationText,
} from '@/utils/environmentRetention'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, recoverEnvironment: vi.fn(), startEnvironment: vi.fn() }
})

const NOW = Date.parse('2026-11-04T00:00:00.000Z')
const EXPIRES = '2026-11-08T12:00:00.000Z'

type Disk = NonNullable<EnvironmentDetailSchema['diskRetention']>

function disk(patch: Partial<Disk> = {}): Disk {
  return {
    state: 'retained',
    startedAt: '2026-11-01T12:00:00.000Z',
    expiresAt: EXPIRES,
    retiredGeneration: 2,
    computeRetiredAt: '2026-11-01T12:01:00.000Z',
    ...patch,
  }
}

function instance(patch: Partial<EnvironmentDetailSchema> = {}): EnvironmentDetailSchema {
  return {
    id: 'env-1',
    displayLabel: '论文实验',
    projectId: 'project-1',
    releaseId: 'release-1',
    releaseVersion: 1,
    runtimeKind: 'container',
    class: 'work',
    revision: 9,
    generation: 2,
    desiredState: 'deleted',
    observedState: 'deleted',
    diskRetention: disk(),
    operation: { id: 'op-delete', kind: 'delete', state: 'succeeded', attempt: 1, maxAttempts: 3 },
    ...patch,
  } as unknown as EnvironmentDetailSchema
}

const ALL_ACTIONS: ObjectAction[] = ['browse', 'edit', 'start', 'stop', 'grant_access']

function permissionsFor(actions: ObjectAction[]): ObjectActionPermissions {
  return {
    current: computed(() => ({ projectId: 'project-1', actorId: 'actor-1', membershipRevision: 1, validUntil: '2099-01-01T00:00:00.000Z', actions })),
    allows: (action) => actions.includes(action),
    denialReason: (action, purpose) => (actions.includes(action) ? null : `职责不包含 ${action}${purpose ? `，不能${purpose}` : ''}`),
    refresh: vi.fn(async () => undefined),
  }
}

function harness(data: EnvironmentDetailSchema, permissions = permissionsFor(ALL_ACTIONS)) {
  const env = reactive({ instance: { kind: 'success', data }, load: vi.fn(async () => undefined) })
  const operations = { load: vi.fn(async () => undefined) }
  const lifecycle = useEnvironmentLifecycle(ref('project-1'))
  const controls = useEnvironmentLifecycleControls({
    env,
    operations,
    lifecycle,
    activeOperation: ref<EnvironmentOperationSnapshotSchema | null>(null),
    retryableOperation: ref<EnvironmentOperationSnapshotSchema | null>(null),
    currentOperationRecovery: ref<OperationRecovery>({ kind: 'pending', reading: false }),
    permissions,
  })
  return { env, operations, lifecycle, controls, permissions }
}

const accepted = { environmentId: 'env-1', operationId: 'op-restore', revision: 10, statusUrl: '/api/v1/environments/env-1/operations/op-restore' }
const recover = vi.mocked(recoverEnvironment)
const sentKeys = () => recover.mock.calls.map(([options]) => (options.headers as Record<string, string>)['Idempotency-Key'])
const sentRevisions = () => recover.mock.calls.map(([options]) => (options.headers as Record<string, string>)['If-Match'])

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(NOW)
  recover.mockReset()
})
afterEach(() => {
  vi.useRealTimers()
})

describe('disk retention read model', () => {
  it('states for each disk state what happened to compute and until when the disk is kept', () => {
    const retiring = instance({ observedState: 'deleting', diskRetention: disk({ state: 'retiring', computeRetiredAt: null }) })
    expect(computeStatusText(retiring, retiring.diskRetention!)).toContain('正在停止计算')
    expect(deadlineText(retiring.diskRetention!, NOW)).toContain('剩余约 4 天 12 小时')

    const retained = instance()
    expect(computeStatusText(retained, retained.diskRetention!)).toContain('第 2 代')
    expect(deadlineText(retained.diskRetention!, NOW)).toContain('到期后平台自动永久清除')
    expect(deadlineText(retained.diskRetention!, Date.parse(EXPIRES) + 1)).toContain('不能再恢复')

    const restored = disk({ state: 'restored', restoredGeneration: 3 })
    expect(computeStatusText(instance({ diskRetention: restored }), restored)).toContain('第 3 代，环境处于已停止状态')
    expect(deadlineText(restored, NOW)).toContain('恢复不延长保留期')

    const purging = disk({ state: 'purging' })
    expect(computeStatusText(instance({ diskRetention: purging }), purging)).toContain('正在永久清除磁盘')

    const purged = disk({ state: 'purged', purgedAt: '2026-11-08T12:05:00.000Z' })
    expect(computeStatusText(instance({ diskRetention: purged }), purged)).toContain('永久清除，无法恢复')
  })

  it('warns before restore that it is a new stopped generation, what does not come back, and that compute is not re-funded', () => {
    const text = restoreConfirmationText(instance())
    expect(text).toContain('第 3 代')
    expect(text).toContain('不会自动启动')
    expect(text).toContain('/workspace 以外的改动')
    expect(text).toContain('不会延长保留期')
    expect(text).toContain('恢复不会重新取得计算资金')
  })

  it('words delete as compute release with seven-day retention and names what purge loses for good', () => {
    const copy = deleteDialogCopy(instance({ desiredState: 'running', observedState: 'ready', diskRetention: null }))
    expect(copy.confirmText).toBe('删除')
    expect(copy.description).toContain('保留七天')
    expect(copy.description).toContain('永久清除工作盘 /workspace及其中全部文件')
    expect(copy.description).not.toContain('不可恢复。项目')
    const vm = deleteDialogCopy(instance({ runtimeKind: 'virtual_machine', desiredState: 'running', observedState: 'ready', diskRetention: null }))
    expect(vm.description).toContain('虚拟机系统盘')
    const again = deleteDialogCopy(instance({ desiredState: 'stopped', observedState: 'stopped', diskRetention: disk({ state: 'restored', restoredGeneration: 3 }) }))
    expect(again.description).toContain('再次删除不会重新开始七天')
  })
})

describe('effective runtime permissions', () => {
  const authorization = {
    defaultContainerUid: 0,
    validUntil: '2026-12-01T00:00:00.000Z',
    authorization: {
      basis: 'project_preauthorization',
      platformConfigurationRevision: 4,
      permissions: { capabilities: ['NET_ADMIN'], privileged: false, scope: { kind: 'project', projectId: 'project-1' } },
    },
  } as unknown as NonNullable<EnvironmentDetailSchema['runtimeAuthorization']>

  it('shows root, approved capabilities and the non-privileged boundary as the receipt reports them', () => {
    const rows = effectivePermissionRows({ runtimeKind: 'container' }, authorization, null)
    const value = (key: string) => rows.find((row) => row.key === key)?.value
    expect(value('user')).toBe('UID 0（root），来自平台默认设置')
    expect(value('capabilities')).toContain('NET_ADMIN')
    expect(value('privileged')).toContain('privileged=false')
    expect(value('boundary')).toContain('不需要')
    expect(value('basis')).toBe('项目预授权')
    expect(rows.find((row) => row.key === 'user')?.elevated).toBe(true)
  })

  it('does not guess the run user when the template declaration is unreadable, and does not apply container settings to VMs', () => {
    const unknown = effectivePermissionRows({ runtimeKind: 'container' }, authorization, undefined)
    expect(unknown.find((row) => row.key === 'user')?.value).toContain('本页没有读到模板声明')
    const vm = effectivePermissionRows({ runtimeKind: 'virtual_machine' }, authorization, undefined)
    expect(vm.find((row) => row.key === 'user')?.value).toContain('guest')
    expect(vm.find((row) => row.key === 'capabilities')?.value).toContain('不适用')
  })

  it('reports a privileged grant without inventing its execution boundary', () => {
    const privileged = { ...authorization, authorization: { ...authorization.authorization, permissions: { ...authorization.authorization.permissions, capabilities: [], privileged: true } } }
    const rows = effectivePermissionRows({ runtimeKind: 'container' }, privileged, 1000)
    expect(rows.find((row) => row.key === 'privileged')?.value).toBe('已批准 privileged')
    expect(rows.find((row) => row.key === 'boundary')?.value).toContain('平台没有返回这个环境的执行边界状态')
    expect(rows.find((row) => row.key === 'user')?.value).toBe('UID 1000，由模板声明')
  })

  it('puts a readable Chinese reason before code and request id, and never offers retry for final refusals', () => {
    const outside = environmentDiagnostic('LW_RUNTIME_PERMISSION_OUTSIDE_PLATFORM_RANGE', undefined, 'x', true, { status: 403, requestId: 'req-1' })
    expect(outside.message).toContain('超出了平台允许的范围')
    expect(outside.code).toBe('LW_RUNTIME_PERMISSION_OUTSIDE_PLATFORM_RANGE')
    expect(outside.requestId).toBe('req-1')
    expect(outside.retryable).toBe(false)
    const funding = environmentDiagnostic('LW_RESOURCE_USAGE_CONFLICT', undefined, 'x', true, { status: 409 })
    expect(funding.message).toContain('还没有为恢复后的环境提供新的资金授权')
    expect(funding.retryable).toBe(false)
    // A server-provided detail stays the reason.
    expect(environmentDiagnostic('LW_ENVIRONMENT_DISK_RETENTION_EXPIRED', '七天磁盘保留期已结束，不能恢复。', 'x', false).message).toBe('七天磁盘保留期已结束，不能恢复。')
    expect(environmentDiagnostic('LW_RUNTIME_AUTHORITY_UNAVAILABLE', undefined, 'x', true).retryable).toBe(true)
  })
})

describe('restore and purge controls', () => {
  it('offers restore only for a retained disk inside its window, with edit permission and no running operation', () => {
    const { controls } = harness(instance())
    expect(controls.diskRecoveryBlockReason(instance(), 'restore')).toBeNull()
    expect(controls.diskRecoveryBlockReason(instance({ diskRetention: disk({ state: 'retiring' }) }), 'restore')).toContain('计算尚未确认停止')
    expect(controls.diskRecoveryBlockReason(instance({ diskRetention: disk({ state: 'purging' }) }), 'restore')).toContain('不能恢复')
    expect(controls.diskRecoveryBlockReason(instance({ diskRetention: disk({ state: 'purged' }) }), 'restore')).toContain('不能恢复')
    expect(controls.diskRecoveryBlockReason(instance(), 'restore', Date.parse(EXPIRES))).toContain('保留期已于')
    expect(controls.diskRecoveryBlockReason(instance({ diskRetention: null }), 'restore')).toContain('没有保留的磁盘')
    expect(controls.diskRecoveryBlockReason(instance({ operation: { id: 'op', kind: 'cleanup', state: 'running' } as never }), 'restore')).toContain('请等待操作完成')
    const { controls: viewer } = harness(instance(), permissionsFor(['browse', 'start']))
    expect(viewer.diskRecoveryBlockReason(instance(), 'restore')).toContain('edit')
    expect(controls.lifecycleActionHint(instance())).toContain('可在概览的“磁盘保留与恢复”中恢复')
  })

  it('submits nothing before confirmation, then one restore with If-Match of the confirmed revision', async () => {
    const data = instance()
    const { controls, env, operations } = harness(data)
    recover.mockResolvedValue({ data: accepted } as never)
    controls.requestDiskRecovery(data, 'restore')
    expect(recover).not.toHaveBeenCalled()
    expect(controls.diskRecoveryConfirmation.value?.intent).toBe('restore')
    await controls.confirmDiskRecovery()
    expect(recover).toHaveBeenCalledTimes(1)
    expect(sentRevisions()).toEqual(['"rev-9"'])
    expect(controls.diskRecoveryAccepted.value).toMatchObject({ intent: 'restore', operationId: 'op-restore' })
    expect(env.load).toHaveBeenCalled()
    expect(operations.load).toHaveBeenCalled()
  })

  it('retries an unconfirmed restore with the same key and revision, so the server replays instead of acting twice', async () => {
    const data = instance()
    const { controls } = harness(data)
    recover
      .mockRejectedValueOnce(new Error('network down'))
      .mockResolvedValueOnce({ error: { diagnosticCode: 'LW_ENVIRONMENT_STORE_UNAVAILABLE', status: 503, retryable: true } } as never)
      .mockResolvedValueOnce({ data: accepted } as never)
    controls.requestDiskRecovery(data, 'restore')
    await controls.confirmDiskRecovery()
    expect(controls.diskRecoveryDiagnostic.value?.message).toContain('请求结果尚未确认')
    await controls.retryDiskRecovery()
    expect(controls.diskRecoveryDiagnostic.value?.code).toBe('LW_ENVIRONMENT_STORE_UNAVAILABLE')
    await controls.retryDiskRecovery()
    expect(recover).toHaveBeenCalledTimes(3)
    expect(new Set(sentKeys()).size).toBe(1)
    expect(new Set(sentRevisions())).toEqual(new Set(['"rev-9"']))
    expect(controls.diskRecoveryDiagnostic.value).toBeNull()
    // The accepted command is done; a later retry does not resend it.
    await controls.retryDiskRecovery()
    expect(recover).toHaveBeenCalledTimes(3)
  })

  it('does not send a second restore while the first is in flight', async () => {
    const data = instance()
    const { controls } = harness(data)
    let resolve: (value: unknown) => void = () => undefined
    recover.mockReturnValueOnce(new Promise((done) => { resolve = done }) as never)
    controls.requestDiskRecovery(data, 'restore')
    const first = controls.confirmDiskRecovery()
    controls.requestDiskRecovery(data, 'restore')
    expect(controls.diskRecoveryConfirmation.value).toBeNull()
    await controls.retryDiskRecovery()
    resolve({ data: accepted })
    await first
    expect(recover).toHaveBeenCalledTimes(1)
  })

  it('shows the server refusal with its readable reason and offers no retry for an expired window', async () => {
    const data = instance()
    const { controls } = harness(data)
    recover.mockResolvedValueOnce({ error: { diagnosticCode: 'LW_ENVIRONMENT_DISK_RETENTION_EXPIRED', detail: '七天磁盘保留期已结束，不能恢复。', status: 409, requestId: 'req-9' } } as never)
    controls.requestDiskRecovery(data, 'restore')
    await controls.confirmDiskRecovery()
    expect(controls.diskRecoveryDiagnostic.value).toMatchObject({ code: 'LW_ENVIRONMENT_DISK_RETENTION_EXPIRED', message: '七天磁盘保留期已结束，不能恢复。', requestId: 'req-9', retryable: false })
  })

  it.each(['restore', 'purge'] as const)('explains an owner-only %s refusal reason first, keeps code and request id, and offers no retry', async (intent) => {
    const data = intent === 'restore'
      ? instance()
      : instance({ observedState: 'failed', diskRetention: disk({ state: 'purging' }), operation: { id: 'op-purge', kind: 'purge', state: 'failed', attempt: 3, maxAttempts: 3 } as never })
    const { controls, permissions } = harness(data)
    recover.mockResolvedValueOnce({ error: { diagnosticCode: 'LW_AUTH_SCOPE_DENIED', status: 403, requestId: 'req-owner' } } as never)
    controls.requestDiskRecovery(data, intent)
    await controls.confirmDiskRecovery()
    expect(controls.diskRecoveryDiagnostic.value).toMatchObject({
      code: 'LW_AUTH_SCOPE_DENIED',
      requestId: 'req-owner',
      status: 403,
      retryable: false,
      message: '只有这个环境的所有者可以恢复磁盘或继续清除磁盘，平台拒绝了这次请求。',
    })
    expect(controls.diskRecoveryDiagnostic.value?.nextStep).toContain('联系环境所有者')
    expect(permissions.refresh).toHaveBeenCalled()
  })

  it('shows the owner-only refusal in the panel before its code and request id', async () => {
    recover.mockResolvedValueOnce({ error: { diagnosticCode: 'LW_AUTH_SCOPE_DENIED', status: 403, requestId: 'req-owner' } } as never)
    const { wrapper, controls } = mountWithContext(EnvironmentDiskRetentionPanel, instance())
    controls.requestDiskRecovery(instance(), 'restore')
    await controls.confirmDiskRecovery()
    await flushPromises()
    const banner = wrapper.get('[data-testid="disk-recovery-diagnostic"]')
    const text = banner.text()
    expect(text.indexOf('只有这个环境的所有者')).toBeGreaterThanOrEqual(0)
    expect(text.indexOf('只有这个环境的所有者')).toBeLessThan(text.indexOf('LW_AUTH_SCOPE_DENIED'))
    expect(text).toContain('req-owner')
    expect(banner.findAll('button').some((button) => button.text().includes('重试'))).toBe(false)
    document.body.innerHTML = ''
  })

  it('drops a confirmation whose revision moved on before it was confirmed', async () => {
    const data = instance()
    const { controls, env } = harness(data)
    controls.requestDiskRecovery(data, 'restore')
    env.instance.data = instance({ revision: 10 })
    await controls.confirmDiskRecovery()
    expect(recover).not.toHaveBeenCalled()
  })

  it('continues only a failed purge, after confirming what is lost', async () => {
    const failed = instance({ observedState: 'failed', diskRetention: disk({ state: 'purging' }), operation: { id: 'op-purge', kind: 'purge', state: 'failed', attempt: 3, maxAttempts: 3, diagnosticCode: 'LW_PROVIDER_UNAVAILABLE' } as never })
    const { controls } = harness(failed)
    expect(controls.diskRecoveryBlockReason(instance(), 'purge')).toContain('只有保留期结束后未完成的清除')
    expect(controls.diskRecoveryBlockReason(failed, 'purge')).toBeNull()
    expect(controls.lifecycleActionHint(failed)).toContain('磁盘清除没有完成')
    recover.mockResolvedValueOnce({ data: { ...accepted, operationId: 'op-recover' } } as never)
    controls.requestDiskRecovery(failed, 'purge')
    expect(recover).not.toHaveBeenCalled()
    await controls.confirmDiskRecovery()
    expect(recover).toHaveBeenCalledTimes(1)
    expect(controls.diskRecoveryAccepted.value?.intent).toBe('purge')
  })

  it('tells a restored, stopped environment that start needs new funding and keeps the original deadline', () => {
    const restored = instance({ desiredState: 'stopped', observedState: 'stopped', generation: 3, diskRetention: disk({ state: 'restored', restoredGeneration: 3 }) })
    const { controls } = harness(restored)
    expect(controls.canStart(restored)).toBe(true)
    expect(controls.lifecycleActionHint(restored)).toContain('恢复不会重新取得计算资金')
    expect(controls.lifecycleActionHint(restored)).toContain('按原期限清除磁盘')
  })

  it('surfaces the stable funding refusal for a start after restore with a readable reason first', async () => {
    const restored = instance({ desiredState: 'stopped', observedState: 'stopped', generation: 3, diskRetention: disk({ state: 'restored', restoredGeneration: 3 }) })
    const { controls } = harness(restored)
    vi.mocked(startEnvironment).mockResolvedValueOnce({ error: { diagnosticCode: 'LW_RESOURCE_USAGE_CONFLICT', status: 409, requestId: 'req-2' } } as never)
    await controls.runLifecycle(restored, 'start')
    expect(controls.lifecycleDiagnostic.value).toMatchObject({ code: 'LW_RESOURCE_USAGE_CONFLICT', requestId: 'req-2', retryable: false })
    expect(controls.lifecycleDiagnostic.value?.message.indexOf('恢复后的环境')).toBe(0)
    expect(controls.lifecycleDiagnostic.value?.nextStep).toContain('重试不会成功')
  })
})

function mountWithContext(component: typeof EnvironmentDiskRetentionPanel | typeof EnvironmentRuntimePermissions, data: EnvironmentDetailSchema, extra: Partial<EnvironmentEntryContext> = {}) {
  const { controls } = harness(data)
  const context = { ...controls, releases: reactive({ releases: { kind: 'empty' } }), ...extra } as unknown as EnvironmentEntryContext
  const Host = defineComponent({
    setup() {
      provideEnvironmentEntry(context)
      return () => h(component, { data })
    },
  })
  return { wrapper: mount(Host, { attachTo: document.body }), controls }
}

describe('EnvironmentDiskRetentionPanel', () => {
  afterEach(() => { document.body.innerHTML = '' })

  it('shows what is kept, what ended with compute and what purge loses, then restores through the confirmation', async () => {
    recover.mockResolvedValueOnce({ data: accepted } as never)
    const { wrapper } = mountWithContext(EnvironmentDiskRetentionPanel, instance())
    const panel = wrapper.get('[data-testid="disk-retention"]')
    expect(panel.attributes('data-disk-state')).toBe('retained')
    expect(panel.text()).toContain('磁盘保留中')
    expect(panel.text()).toContain('计算已于')
    expect(panel.get('[data-testid="retention-inventory"]').text()).toContain('工作盘 /workspace 中的全部文件')
    expect(panel.text()).toContain('存储费用由平台承担')
    await panel.get('[data-testid="restore-disk"]').trigger('click')
    await flushPromises()
    expect(recover).not.toHaveBeenCalled()
    const dialog = document.body.querySelector<HTMLElement>('[role="alertdialog"]')
    expect(dialog?.textContent).toContain('恢复不会重新取得计算资金')
    dialog!.querySelector<HTMLButtonElement>('.filled-button')!.click()
    await flushPromises()
    expect(recover).toHaveBeenCalledTimes(1)
    expect(wrapper.get('[data-testid="disk-recovery-accepted"]').text()).toContain('不会自动启动')
  })

  it('disables restore and says why once the window ended', () => {
    vi.setSystemTime(Date.parse(EXPIRES) + 60_000)
    const { wrapper } = mountWithContext(EnvironmentDiskRetentionPanel, instance())
    expect(wrapper.get('[data-testid="restore-disk"]').attributes('disabled')).toBeDefined()
    expect(wrapper.get('[data-testid="restore-blocked"]').text()).toContain('七天保留期已于')
  })

  it('shows purge progress from server facts only, without percentages', () => {
    const purging = instance({ diskRetention: disk({ state: 'purging' }), operation: { id: 'op-purge', kind: 'purge', state: 'running', attempt: 1, maxAttempts: 3 } as never })
    const { wrapper } = mountWithContext(EnvironmentDiskRetentionPanel, purging)
    const steps = wrapper.get('[data-testid="retention-steps"]').text()
    expect(steps).toContain('处理中，第 1/3 次尝试')
    expect(steps).toContain('尚未完成')
    expect(steps).not.toMatch(/%/)
    expect(wrapper.find('[data-testid="restore-disk"]').exists()).toBe(false)
  })

  it('shows a failed purge reason first and offers to continue it', () => {
    const failed = instance({ observedState: 'failed', diskRetention: disk({ state: 'purging' }), operation: { id: 'op-purge', kind: 'purge', state: 'failed', attempt: 3, maxAttempts: 3, diagnosticCode: 'LW_ENVIRONMENT_CLEANUP_PROTOCOL_UPGRADE_REQUIRED' } as never })
    const { wrapper } = mountWithContext(EnvironmentDiskRetentionPanel, failed)
    const banner = wrapper.get('[data-testid="purge-failure"]').text()
    expect(banner.indexOf('旧版本的清理操作')).toBeLessThan(banner.indexOf('LW_ENVIRONMENT_CLEANUP_PROTOCOL_UPGRADE_REQUIRED'))
    expect(wrapper.get('[data-testid="continue-purge"]').attributes('disabled')).toBeUndefined()
  })

  it('states a purged disk is gone and offers no action', () => {
    const { wrapper } = mountWithContext(EnvironmentDiskRetentionPanel, instance({ diskRetention: disk({ state: 'purged', purgedAt: '2026-11-08T12:05:00.000Z' }) }))
    expect(wrapper.text()).toContain('永久清除，无法恢复')
    expect(wrapper.text()).toContain('评测成绩')
    expect(wrapper.find('button').exists()).toBe(false)
  })

  it('says a deleted environment without retention has no disk', () => {
    const { wrapper } = mountWithContext(EnvironmentDiskRetentionPanel, instance({ diskRetention: null }))
    expect(wrapper.get('[data-testid="retention-none"]').text()).toContain('没有为它保留磁盘')
  })

  it('stays hidden for a running environment without retention and after the restored generation ran again', () => {
    expect(mountWithContext(EnvironmentDiskRetentionPanel, instance({ desiredState: 'running', observedState: 'ready', diskRetention: null })).wrapper.find('[data-testid="disk-retention"]').exists()).toBe(false)
    const reactivated = instance({ desiredState: 'running', observedState: 'ready', diskRetention: disk({ state: 'restored', reactivatedAt: '2026-11-05T00:00:00.000Z' }) })
    expect(mountWithContext(EnvironmentDiskRetentionPanel, reactivated).wrapper.find('[data-testid="disk-retention"]').exists()).toBe(false)
  })
})

describe('EnvironmentRuntimePermissions', () => {
  afterEach(() => { document.body.innerHTML = '' })

  const runtimeAuthorization = {
    defaultContainerUid: 0,
    validUntil: '2026-12-01T00:00:00.000Z',
    authorization: { basis: 'restricted_baseline', platformConfigurationRevision: 2, permissions: { capabilities: [], privileged: false, scope: { kind: 'project', projectId: 'project-1' } } },
  }

  it('lists the reported permissions and reads an explicit run user from the manual release', () => {
    const data = instance({ desiredState: 'running', observedState: 'ready', runtimeAuthorization } as never)
    const releases = reactive({ releases: { kind: 'success', data: [{ id: 'release-1', version: 1, manualApproval: { spec: { security: { runAsUser: 1000 } } } }] } })
    const { wrapper } = mountWithContext(EnvironmentRuntimePermissions, data, { releases } as never)
    const text = wrapper.text()
    expect(text).toContain('UID 1000，由模板声明')
    expect(text).toContain('无：全部丢弃')
    expect(text).toContain('未启用（privileged=false）')
    expect(text).toContain('平台基线')
  })

  it('says the receipt is missing instead of guessing', () => {
    const { wrapper } = mountWithContext(EnvironmentRuntimePermissions, instance({ runtimeAuthorization: null }))
    expect(wrapper.get('[data-testid="runtime-permissions-missing"]').text()).toContain('本页不推测')
  })

  it('shows a recorded runtime refusal with the Chinese reason before the code', () => {
    const data = instance({
      desiredState: 'stopped',
      observedState: 'failed',
      runtimeAuthorization,
      operation: { id: 'op-start', kind: 'start', state: 'failed', attempt: 1, maxAttempts: 3, diagnosticCode: 'LW_RUNTIME_PERMISSION_APPROVAL_REQUIRED' },
    } as never)
    const { wrapper } = mountWithContext(EnvironmentRuntimePermissions, data)
    const banner = wrapper.get('[data-testid="runtime-permission-rejection"]').text()
    expect(banner.indexOf('没有对应的项目或课程预授权')).toBeGreaterThanOrEqual(0)
    expect(banner.indexOf('没有对应的项目或课程预授权')).toBeLessThan(banner.indexOf('LW_RUNTIME_PERMISSION_APPROVAL_REQUIRED'))
  })
})
