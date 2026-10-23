import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  applyPlatformConfiguration,
  getPlatformConfiguration,
  getPlatformConfigurationApply,
  registerPlatformConfigurationBundle,
  updatePlatformConfiguration,
} from '@/generated/contracts'
import type { PlatformConfigurationApply, PlatformConfigurationDesired, PlatformConfigurationStatusSchema } from '@/generated/contracts'
import { POLICY_UNSUPPORTED_CODE, usePlatformConfiguration } from '@/composables/usePlatformConfiguration'
import { requestToYaml } from '@/utils/platformConfiguration'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getPlatformConfiguration: vi.fn(),
    updatePlatformConfiguration: vi.fn(),
    applyPlatformConfiguration: vi.fn(),
    registerPlatformConfigurationBundle: vi.fn(),
    getPlatformConfigurationApply: vi.fn(),
    getPlatformConfigurationVersion: vi.fn(),
  }
})

const defaults: PlatformConfigurationDesired = {
  lifecycle: { readOnlyRetentionSeconds: 604800, detailedLogRetentionSeconds: 2592000, expiryReminderSeconds: [86400, 3600], preemptionGraceSeconds: 60 },
  runtimePolicy: { defaultContainerUid: 0, allowedCapabilities: [], preauthorizations: [], privilegedAllowed: false, privilegedExecutionLocation: null },
  builderPolicy: { capabilities: [], allowPrivilegeEscalation: false, entitlements: [] },
}

const noImpact = { runtimePolicyChanged: false, builderPolicyChanged: false, lifecyclePolicyChanged: false }
const configurationSha = 'a'.repeat(64)
const bundleSha = 'c'.repeat(64)

function unconfigured(): PlatformConfigurationStatusSchema {
  return { revision: 0, appliedState: 'unconfigured', defaults, desired: null, applied: null, registeredBundle: null, latestApply: null, impact: noImpact }
}

function configured(revision: number, grace: number, withRegistration = false, configuration = structuredClone(defaults)): PlatformConfigurationStatusSchema {
  configuration.lifecycle.preemptionGraceSeconds = grace
  return {
    ...unconfigured(),
    revision,
    desired: { revision, configuration, configurationSha256: configurationSha, createdAt: '2026-09-14T08:00:00Z', createdBy: 'admin-a' },
    registeredBundle: withRegistration
      ? {
          desiredRevision: revision,
          configurationSha256: configurationSha,
          encryptedBundleSha256: bundleSha,
          encryptedBytes: 2048,
          registeredAtRevision: revision,
          registeredBy: 'admin-a',
          registeredAt: '2026-09-14T08:05:00Z',
        }
      : null,
    impact: { ...noImpact, lifecyclePolicyChanged: true },
  }
}

function ok<T>(data: T, etag?: string) {
  return { data, error: undefined, response: { headers: etag ? { etag } : {} } }
}

/** Control's real problem body: a generic English `detail`, the diagnostic code and requestId. */
function problem(status: number, code = 'LW_TEST', retryable = false) {
  return {
    data: undefined,
    error: {
      type: `urn:labweaver:problem:${code.toLowerCase()}`,
      title: 'Control request blocked',
      status,
      detail: 'The request was rejected by a fail-closed control-plane boundary.',
      instance: 'urn:labweaver:request:req-1',
      requestId: 'req-1',
      diagnosticCode: code,
      retryable,
    },
  }
}

function ageFile(name = 'platform.age', size = 2048): File {
  return new File([new Uint8Array(size)], name, { type: 'application/octet-stream', lastModified: 1_700_000_000_000 })
}

const getMock = vi.mocked(getPlatformConfiguration)
const updateMock = vi.mocked(updatePlatformConfiguration)
const applyMock = vi.mocked(applyPlatformConfiguration)
const registerMock = vi.mocked(registerPlatformConfigurationBundle)
const getApplyMock = vi.mocked(getPlatformConfigurationApply)

type CallOptions = {
  headers: Record<string, string>
  body: unknown
  bodySerializer?: (b: unknown) => unknown
  path?: { revision: number }
  query?: { expectedRevision: number; configurationSha256: string }
}
function callOf(mock: { mock: { calls: unknown[][] } }, index = 0): CallOptions {
  return mock.mock.calls[index][0] as CallOptions
}
function keysOf(mock: { mock: { calls: unknown[][] } }): string[] {
  return mock.mock.calls.map((call) => (call[0] as CallOptions).headers['Idempotency-Key'])
}

beforeEach(() => {
  vi.resetAllMocks()
})

describe('usePlatformConfiguration', () => {
  it('copies server defaults into the draft when unconfigured and submits the full configuration at revision 0', async () => {
    getMock.mockResolvedValue(ok(unconfigured(), '"rev-0"') as never)
    updateMock.mockResolvedValue(ok(configured(2, 60), '"rev-2"') as never)
    const config = usePlatformConfiguration()
    await config.load()

    expect(config.draft).toEqual(defaults)
    expect(config.baseRevision).toBe(0)
    expect(await config.submitUpdate('form')).toBe(true)

    const call = callOf(updateMock)
    expect(call.body).toEqual({ expectedRevision: 0, configuration: defaults })
    expect(call.headers['If-Match']).toBe('"rev-0"')
    expect(call.headers['Idempotency-Key']).toBeTruthy()
    expect(config.baseRevision).toBe(2)
    expect(config.notice).toContain('保存不等于部署')
  })

  it('reuses the idempotency key for a retried identical request and changes it with the request', async () => {
    getMock.mockResolvedValue(ok(configured(3, 60)) as never)
    updateMock.mockResolvedValue(problem(503, 'LW_PLATFORM_CONFIGURATION_PERSISTENCE_FAILED', true) as never)
    const config = usePlatformConfiguration()
    await config.load()
    config.draft!.lifecycle.preemptionGraceSeconds = 90

    await config.submitUpdate('form')
    await config.submitUpdate('form')
    config.draft!.lifecycle.preemptionGraceSeconds = 95
    await config.submitUpdate('form')

    const keys = keysOf(updateMock)
    expect(keys[0]).toBe(keys[1])
    expect(keys[2]).not.toBe(keys[0])
    expect(config.draft!.lifecycle.preemptionGraceSeconds).toBe(95)
    expect(config.actionFailure).toMatchObject({
      action: 'save',
      diagnostic: { code: 'LW_PLATFORM_CONFIGURATION_PERSISTENCE_FAILED', requestId: 'req-1', retryable: true, message: '平台配置存储暂时不可用。' },
    })
  })

  it.each([
    [409, 'LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT'],
    [412, 'LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT'],
  ])('keeps the draft on a %i revision fence failure and offers rebase against the latest facts', async (status, code) => {
    getMock.mockResolvedValueOnce(ok(configured(3, 60)) as never).mockResolvedValueOnce(ok(configured(7, 30)) as never)
    updateMock.mockResolvedValue(problem(status, code) as never)
    const config = usePlatformConfiguration()
    await config.load()
    config.draft!.lifecycle.preemptionGraceSeconds = 90

    expect(await config.submitUpdate('form')).toBe(false)
    expect(config.conflict?.latest.revision).toBe(7)
    expect(config.actionFailure?.diagnostic).toMatchObject({ code, message: '配置已被其他管理员更新，本次提交未生效。' })
    expect(config.draft!.lifecycle.preemptionGraceSeconds).toBe(90)

    config.rebaseDraftOnLatest()
    expect(config.baseRevision).toBe(7)
    expect(config.draft!.lifecycle.preemptionGraceSeconds).toBe(90)
    expect(config.conflict).toBeNull()
    expect(config.actionFailure).toBeNull()
  })

  it('treats an idempotency conflict as its own failure: no revision comparison, and the next submit uses a new key', async () => {
    getMock.mockResolvedValue(ok(configured(3, 60)) as never)
    updateMock.mockResolvedValue(problem(409, 'LW_IDEMPOTENCY_CONFLICT') as never)
    const config = usePlatformConfiguration()
    await config.load()
    config.draft!.lifecycle.preemptionGraceSeconds = 90

    expect(await config.submitUpdate('form')).toBe(false)
    expect(config.conflict).toBeNull()
    expect(getMock).toHaveBeenCalledTimes(1)
    expect(config.actionFailure?.diagnostic).toMatchObject({
      code: 'LW_IDEMPOTENCY_CONFLICT',
      message: expect.stringContaining('同一请求标识'),
      nextStep: expect.stringContaining('新的请求标识'),
    })

    await config.submitUpdate('form')
    const keys = keysOf(updateMock)
    expect(keys[1]).not.toBe(keys[0])
  })

  it('does not let an older replayed response overwrite newer facts', async () => {
    getMock.mockResolvedValue(ok(configured(8, 60)) as never)
    updateMock.mockResolvedValue(ok(configured(5, 45)) as never)
    const config = usePlatformConfiguration()
    await config.load()
    config.draft!.lifecycle.preemptionGraceSeconds = 45

    await config.submitUpdate('form')
    expect(config.status?.revision).toBe(8)
    expect(config.notice).toContain('回放')
  })

  it('sends advanced YAML verbatim as application/yaml and blocks a mismatched revision locally', async () => {
    getMock.mockResolvedValue(ok(configured(3, 60)) as never)
    updateMock.mockResolvedValue(ok(configured(4, 60)) as never)
    const config = usePlatformConfiguration()
    await config.load()

    const stale = requestToYaml({ expectedRevision: 2, configuration: config.draft! })
    expect(await config.submitUpdate('yaml', stale)).toBe(false)
    expect(updateMock).not.toHaveBeenCalled()
    expect(config.actionFailure?.diagnostic.code).toBe('LW_PLATFORM_CONFIGURATION_REVISION_MISMATCH')

    const yaml = requestToYaml({ expectedRevision: 3, configuration: config.draft! })
    expect(await config.submitUpdate('yaml', yaml)).toBe(true)
    const call = callOf(updateMock)
    expect(call.headers['Content-Type']).toBe('application/yaml')
    expect(call.bodySerializer?.(call.body)).toBe(yaml)
  })

  it.each([
    ['runtime capabilities', (c: PlatformConfigurationDesired) => { c.runtimePolicy.allowedCapabilities = ['NET_ADMIN'] }, '普通容器 capabilities 允许清单'],
    ['preauthorizations', (c: PlatformConfigurationDesired) => { c.runtimePolicy.preauthorizations = [{ scope: { kind: 'project', projectId: 'p-1' }, capabilities: [], privileged: false }] }, '项目/课程预授权'],
    ['VM guest location', (c: PlatformConfigurationDesired) => { c.runtimePolicy.privilegedAllowed = true; c.runtimePolicy.privilegedExecutionLocation = { kind: 'virtual_machine_guest', providerBinding: 'kubevirt' } }, 'VM guest 执行位置'],
    ['builder entitlements', (c: PlatformConfigurationDesired) => { c.builderPolicy.entitlements = ['network.host'] }, 'builder entitlements'],
    ['builder capabilities', (c: PlatformConfigurationDesired) => { c.builderPolicy.capabilities = ['SYS_ADMIN'] }, 'builder capabilities'],
    ['builder escalation', (c: PlatformConfigurationDesired) => { c.builderPolicy.allowPrivilegeEscalation = true }, 'builder 提权'],
    ['non-zero container UID', (c: PlatformConfigurationDesired) => { c.runtimePolicy.defaultContainerUid = 1000 }, '默认容器 UID'],
  ])('fails closed on %s without sending YAML that the deployment path would reject', async (_name, mutate, label) => {
    getMock.mockResolvedValue(ok(configured(3, 60)) as never)
    const config = usePlatformConfiguration()
    await config.load()
    const configuration = structuredClone(defaults)
    mutate(configuration)

    expect(await config.submitUpdate('yaml', requestToYaml({ expectedRevision: 3, configuration }))).toBe(false)
    expect(updateMock).not.toHaveBeenCalled()
    expect(config.actionFailure?.diagnostic).toMatchObject({
      code: POLICY_UNSUPPORTED_CODE,
      message: expect.stringContaining(label),
      nextStep: expect.stringContaining('#207'),
    })
  })

  it('accepts privileged runtime on a dedicated node pool, the one execution-side consumer', async () => {
    getMock.mockResolvedValue(ok(configured(3, 60)) as never)
    updateMock.mockResolvedValue(ok(configured(4, 60)) as never)
    const config = usePlatformConfiguration()
    await config.load()
    config.draft!.runtimePolicy.privilegedAllowed = true
    config.draft!.runtimePolicy.privilegedExecutionLocation = { kind: 'dedicated_node_pool', nodeSelector: { pool: 'privileged' }, taintKey: 'labweaver.io/privileged', taintValue: 'true' }

    expect(config.draftUnsupported).toEqual([])
    expect(await config.submitUpdate('form')).toBe(true)
    expect(updateMock).toHaveBeenCalledTimes(1)
  })

  it('shows the Chinese reason for a server policy rejection before the code and requestId', async () => {
    getMock.mockResolvedValue(ok(configured(3, 60)) as never)
    updateMock.mockResolvedValue(problem(422, 'LW_PLATFORM_CONFIGURATION_EXECUTION_LOCATION_INVALID') as never)
    const config = usePlatformConfiguration()
    await config.load()
    config.draft!.runtimePolicy.privilegedAllowed = true
    config.draft!.runtimePolicy.privilegedExecutionLocation = { kind: 'dedicated_node_pool', nodeSelector: {}, taintKey: '', taintValue: '' }

    expect(await config.submitUpdate('form')).toBe(false)
    const diagnostic = config.actionFailure!.diagnostic
    expect(diagnostic.message).toBe('特权执行位置无效。')
    expect(diagnostic.message).not.toContain('fail-closed')
    expect(diagnostic.nextStep).toContain('专用节点池')
    expect(diagnostic).toMatchObject({ code: 'LW_PLATFORM_CONFIGURATION_EXECUTION_LOCATION_INVALID', requestId: 'req-1', status: 422 })
    expect(config.draft!.runtimePolicy.privilegedAllowed).toBe(true)
  })

  it('registers a real .age file using server revision and configurationSha256, then applies without a hand-filled digest', async () => {
    const previous: PlatformConfigurationApply = {
      id: 'apply-1', desiredRevision: 2, encryptedBundleSha256: 'b'.repeat(64), stage: 'failed', failedStage: 'applying',
      failureReason: 'external_result_unknown', nextStep: 'reconcile_external_result', requestedAt: '2026-09-14T08:00:00Z',
      requestedBy: 'admin-a', revision: 3, updatedAt: '2026-09-14T08:10:00Z',
    }
    const latest: PlatformConfigurationApply = {
      ...previous, id: 'apply-2', desiredRevision: 3, stage: 'requested', nextStep: 'execute_approved_deployment',
      failedStage: null, failureReason: null, previousApplyId: 'apply-1', revision: 1, encryptedBundleSha256: bundleSha,
    }
    const registered = { ...configured(3, 60, true), revision: 4 }
    getMock.mockResolvedValue(ok(configured(3, 60), '"rev-3"') as never)
    registerMock.mockResolvedValue(ok(registered, '"rev-4"') as never)
    applyMock.mockResolvedValue(ok({ ...registered, revision: 5, latestApply: latest }, '"rev-5"') as never)
    getApplyMock.mockResolvedValue(ok(previous) as never)
    const config = usePlatformConfiguration()
    await config.load()

    const file = ageFile()
    config.selectBundleFile(file)
    expect(config.canRegister).toBe(true)
    expect(await config.registerBundle()).toBe(true)
    const registerCall = callOf(registerMock)
    expect(registerCall.path).toEqual({ revision: 3 })
    expect(registerCall.query).toEqual({ expectedRevision: 3, configurationSha256: configurationSha })
    expect(registerCall.body).toBe(file)
    expect(registerCall.headers['If-Match']).toBe('"rev-3"')
    expect(config.status?.registeredBundle?.encryptedBundleSha256).toBe(bundleSha)

    expect(await config.requestApply()).toBe(true)
    const applyCall = callOf(applyMock)
    expect(applyCall.body).toEqual({ expectedRevision: 4, desiredRevision: 3 })
    expect(applyCall.body).not.toHaveProperty('encryptedBundleSha256')
    expect(applyCall.headers['If-Match']).toBe('"rev-4"')
    expect(config.notice).toContain('202')
    expect(config.status?.applied ?? null).toBeNull()

    await config.loadPreviousApply()
    expect(getApplyMock).toHaveBeenCalledWith({ path: { applyId: 'apply-1' } })
    expect(config.applyHistory.map((item) => item.id)).toEqual(['apply-2', 'apply-1'])
  })

  it('reuses the register idempotency key for the same file and keeps the file on a revision conflict', async () => {
    getMock
      .mockResolvedValueOnce(ok(configured(3, 60), '"rev-3"') as never)
      .mockResolvedValue(ok(configured(7, 60), '"rev-7"') as never)
    registerMock.mockResolvedValue(problem(412, 'LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT') as never)
    const config = usePlatformConfiguration()
    await config.load()
    const file = ageFile()
    config.selectBundleFile(file)

    expect(await config.registerBundle()).toBe(false)
    config.conflict = null
    expect(await config.registerBundle()).toBe(false)
    const keys = keysOf(registerMock)
    expect(keys[0]).toBe(keys[1])
    expect(config.selectedBundleFile).toBe(file)
    expect(config.conflict?.latest.revision).toBe(7)
    expect(config.actionFailure?.diagnostic.nextStep).toContain('.age 文件已保留')
  })

  it('refuses to register or apply a saved desired that carries settings without an execution-side consumer', async () => {
    const configuration = structuredClone(defaults)
    configuration.runtimePolicy.allowedCapabilities = ['SYS_PTRACE']
    getMock.mockResolvedValue(ok(configured(3, 60, true, configuration), '"rev-3"') as never)
    const config = usePlatformConfiguration()
    await config.load()
    config.selectBundleFile(ageFile())

    expect(config.desiredUnsupported.map((field) => field.path)).toEqual(['runtimePolicy.allowedCapabilities'])
    expect(config.canRegister).toBe(false)
    expect(config.canApply).toBe(false)
    expect(await config.registerBundle()).toBe(false)
    expect(registerMock).not.toHaveBeenCalled()
    expect(config.actionFailure).toMatchObject({ action: 'register', diagnostic: { code: POLICY_UNSUPPORTED_CODE } })
  })

  it('blocks apply without registration and explains a server-side LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED', async () => {
    getMock.mockResolvedValue(ok(configured(3, 60), '"rev-3"') as never)
    applyMock.mockResolvedValue(problem(409, 'LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED') as never)
    const config = usePlatformConfiguration()
    await config.load()

    expect(config.canApply).toBe(false)
    expect(await config.requestApply()).toBe(false)
    expect(applyMock).not.toHaveBeenCalled()
    expect(config.actionFailure?.diagnostic.code).toBe('LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED')

    // The registration was superseded on the server after it was read: the 409 is not a revision conflict.
    config.status = configured(3, 60, true)
    expect(await config.requestApply()).toBe(false)
    expect(applyMock).toHaveBeenCalledTimes(1)
    expect(config.conflict).toBeNull()
    expect(config.actionFailure?.diagnostic).toMatchObject({
      message: '当前 desired 没有有效的 Bundle 登记，服务端拒绝应用请求。',
      nextStep: expect.stringContaining('.age'),
    })
  })

  it('reports an apply already in progress without opening a revision comparison', async () => {
    getMock.mockResolvedValue(ok(configured(3, 60, true), '"rev-3"') as never)
    applyMock.mockResolvedValue(problem(409, 'LW_PLATFORM_CONFIGURATION_APPLY_IN_PROGRESS') as never)
    const config = usePlatformConfiguration()
    await config.load()

    expect(await config.requestApply()).toBe(false)
    expect(config.conflict).toBeNull()
    expect(config.actionFailure?.diagnostic).toMatchObject({ message: expect.stringContaining('上一次应用尝试尚未结束'), nextStep: expect.stringContaining('终态') })
  })

  it('explains a forbidden read before showing identifiers', async () => {
    getMock.mockResolvedValue(problem(403, 'LW_FORBIDDEN') as never)
    const config = usePlatformConfiguration()
    await config.load()
    expect(config.loadState).toBe('error')
    expect(config.loadFailure).toMatchObject({
      code: 'LW_FORBIDDEN',
      message: expect.stringContaining('平台管理员'),
      nextStep: expect.stringContaining('平台管理员账号'),
    })
  })
})
