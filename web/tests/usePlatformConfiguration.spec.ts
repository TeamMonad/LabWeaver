import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  applyPlatformConfiguration,
  getPlatformConfiguration,
  getPlatformConfigurationApply,
  registerPlatformConfigurationBundle,
  updatePlatformConfiguration,
} from '@/generated/contracts'
import type { PlatformConfigurationApply, PlatformConfigurationDesired, PlatformConfigurationStatusSchema } from '@/generated/contracts'
import { usePlatformConfiguration } from '@/composables/usePlatformConfiguration'
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

function configured(revision: number, grace: number, withRegistration = false): PlatformConfigurationStatusSchema {
  const configuration = structuredClone(defaults)
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

function problem(status: number, code = 'LW_TEST') {
  return { data: undefined, error: { type: 'about:blank', title: 't', status, detail: `失败 ${status}`, instance: '/x', requestId: 'req-1', diagnosticCode: code, retryable: false } }
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
    updateMock.mockResolvedValue(problem(503) as never)
    const config = usePlatformConfiguration()
    await config.load()
    config.draft!.lifecycle.preemptionGraceSeconds = 90

    await config.submitUpdate('form')
    await config.submitUpdate('form')
    config.draft!.lifecycle.preemptionGraceSeconds = 95
    await config.submitUpdate('form')

    const keys = updateMock.mock.calls.map((call) => (call[0] as CallOptions).headers['Idempotency-Key'])
    expect(keys[0]).toBe(keys[1])
    expect(keys[2]).not.toBe(keys[0])
    expect(config.draft!.lifecycle.preemptionGraceSeconds).toBe(95)
    expect(config.actionFailure).toMatchObject({ code: 'LW_TEST', requestId: 'req-1' })
  })

  it('keeps the draft on a revision conflict and offers rebase or discard against the latest facts', async () => {
    getMock.mockResolvedValueOnce(ok(configured(3, 60)) as never).mockResolvedValueOnce(ok(configured(7, 30)) as never)
    updateMock.mockResolvedValue(problem(409) as never)
    const config = usePlatformConfiguration()
    await config.load()
    config.draft!.lifecycle.preemptionGraceSeconds = 90

    expect(await config.submitUpdate('form')).toBe(false)
    expect(config.conflict?.latest.revision).toBe(7)
    expect(config.draft!.lifecycle.preemptionGraceSeconds).toBe(90)

    config.rebaseDraftOnLatest()
    expect(config.baseRevision).toBe(7)
    expect(config.draft!.lifecycle.preemptionGraceSeconds).toBe(90)
    expect(config.conflict).toBeNull()
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
    expect(config.actionFailure?.code).toBe('LW_PLATFORM_CONFIGURATION_REVISION_MISMATCH')

    const yaml = requestToYaml({ expectedRevision: 3, configuration: config.draft! })
    expect(await config.submitUpdate('yaml', yaml)).toBe(true)
    const call = callOf(updateMock)
    expect(call.headers['Content-Type']).toBe('application/yaml')
    expect(call.bodySerializer?.(call.body)).toBe(yaml)
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
    expect(await config.registerBundle(file)).toBe(true)
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

  it('reuses the register idempotency key for the same file and keeps the file on 409', async () => {
    getMock
      .mockResolvedValueOnce(ok(configured(3, 60), '"rev-3"') as never)
      .mockResolvedValue(ok(configured(7, 60), '"rev-7"') as never)
    registerMock.mockResolvedValue(problem(409) as never)
    const config = usePlatformConfiguration()
    await config.load()
    const file = ageFile()

    expect(await config.registerBundle(file)).toBe(false)
    expect(await config.registerBundle(file)).toBe(false)
    const keys = registerMock.mock.calls.map((call) => (call[0] as CallOptions).headers['Idempotency-Key'])
    expect(keys[0]).toBe(keys[1])
    expect(config.selectedBundleFile).toBe(file)
    expect(config.conflict?.latest.revision).toBe(7)
  })

  it('blocks apply without registration and maps LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED', async () => {
    getMock.mockResolvedValue(ok(configured(3, 60), '"rev-3"') as never)
    applyMock.mockResolvedValue(problem(422, 'LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED') as never)
    const config = usePlatformConfiguration()
    await config.load()

    expect(await config.requestApply()).toBe(false)
    expect(applyMock).not.toHaveBeenCalled()
    expect(config.actionFailure?.code).toBe('LW_PLATFORM_CONFIGURATION_BUNDLE_REQUIRED')

    // Force a server-side required error after a stale local registration check by stubbing canApply path:
    // mutate status to look registered then have server reject.
    config.status = configured(3, 60, true)
    expect(await config.requestApply()).toBe(false)
    expect(applyMock).toHaveBeenCalledTimes(1)
    expect(config.actionFailure?.recovery).toContain('.age')
  })

  it('explains a forbidden read before showing identifiers', async () => {
    getMock.mockResolvedValue(problem(403, 'LW_FORBIDDEN') as never)
    const config = usePlatformConfiguration()
    await config.load()
    expect(config.loadState).toBe('error')
    expect(config.loadFailure).toMatchObject({ code: 'LW_FORBIDDEN', recovery: expect.stringContaining('平台管理员') })
  })
})
