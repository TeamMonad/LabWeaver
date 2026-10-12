import { activeScene, fixtureProjectId } from './scenes'
import type {
  PlatformConfigurationApply,
  PlatformConfigurationDesired,
  PlatformConfigurationStatusSchema,
  PlatformConfigurationVersionSchema,
  ProblemDetails,
} from '../src/generated/contracts/types.gen'

type Result<T> = { data: T; error: undefined } | { data: undefined; error: ProblemDetails }

function preview<T>(data: T): Promise<Result<T>> {
  return Promise.resolve({ data, error: undefined })
}

function previewProblem(diagnosticCode: 'FIXTURE_OPERATION_UNSUPPORTED' | 'FIXTURE_RESOURCE_NOT_FOUND', detail: string, status: 404 | 501): Promise<Result<never>> {
  return Promise.resolve({
    data: undefined,
    error: {
      type: 'about:blank',
      title: diagnosticCode === 'FIXTURE_OPERATION_UNSUPPORTED' ? '预览操作未覆盖' : '预览资源不存在',
      status,
      detail,
      instance: '/fixture-preview',
      requestId: 'fixture-' + diagnosticCode.toLowerCase(),
      diagnosticCode,
      retryable: false,
    },
  })
}

function unsupportedResponse(operation: string) {
  return previewProblem('FIXTURE_OPERATION_UNSUPPORTED', 'Fixture 预览未覆盖 ' + operation + '，不能执行真实写操作。', 501)
}

function notFoundResponse(resourceName: string, id: string) {
  return previewProblem('FIXTURE_RESOURCE_NOT_FOUND', 'Fixture 中不存在' + resourceName + '：' + id, 404)
}

// Platform configuration (#203): read-only preview facts. Writes stay unsupported in the fixture.
const platformConfigurationDefaults = {
  lifecycle: { readOnlyRetentionSeconds: 604800, detailedLogRetentionSeconds: 2592000, expiryReminderSeconds: [86400, 3600], preemptionGraceSeconds: 60 },
  runtimePolicy: { defaultContainerUid: 0, allowedCapabilities: [], preauthorizations: [], privilegedAllowed: false, privilegedExecutionLocation: null },
  builderPolicy: { capabilities: [], allowPrivilegeEscalation: false, entitlements: [] },
} satisfies PlatformConfigurationDesired

const platformConfigurationApplied = {
  revision: 3,
  configuration: platformConfigurationDefaults,
  configurationSha256: '3f1d0c9a7b2e4c6d8e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d',
  createdAt: '2026-09-10T02:00:00.000Z',
  createdBy: 'fixture-admin',
} satisfies PlatformConfigurationVersionSchema

const platformConfigurationDesired = {
  revision: 5,
  configuration: {
    lifecycle: { readOnlyRetentionSeconds: 1209600, detailedLogRetentionSeconds: 2592000, expiryReminderSeconds: [86400, 3600], preemptionGraceSeconds: 120 },
    runtimePolicy: {
      defaultContainerUid: 1000,
      allowedCapabilities: ['NET_BIND_SERVICE', 'SYS_PTRACE'],
      preauthorizations: [{ scope: { kind: 'project', projectId: fixtureProjectId }, capabilities: ['SYS_PTRACE'], privileged: false }],
      privilegedAllowed: true,
      privilegedExecutionLocation: { kind: 'dedicated_node_pool', nodeSelector: { 'labweaver.io/pool': 'privileged' }, taintKey: 'labweaver.io/privileged', taintValue: 'true' },
    },
    builderPolicy: { capabilities: [], allowPrivilegeEscalation: false, entitlements: [] },
  },
  configurationSha256: '9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b',
  createdAt: '2026-09-14T06:30:00.000Z',
  createdBy: 'fixture-admin',
} satisfies PlatformConfigurationVersionSchema

const platformConfigurationPreviousApply = {
  id: '0192f0a0-0000-7000-8000-000000000201',
  desiredRevision: 3,
  encryptedBundleSha256: 'b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2',
  executorClientId: 'deploy-executor',
  stage: 'succeeded',
  nextStep: 'none',
  requestedAt: '2026-09-10T02:05:00.000Z',
  requestedBy: 'fixture-admin',
  revision: 4,
  updatedAt: '2026-09-10T02:20:00.000Z',
} satisfies PlatformConfigurationApply

const platformConfigurationLatestApply = {
  id: '0192f0a0-0000-7000-8000-000000000305',
  desiredRevision: 5,
  encryptedBundleSha256: 'c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3',
  executorClientId: 'deploy-executor',
  stage: 'failed',
  failedStage: 'applying',
  failureReason: 'external_result_unknown',
  nextStep: 'reconcile_external_result',
  previousApplyId: platformConfigurationPreviousApply.id,
  requestedAt: '2026-09-14T07:00:00.000Z',
  requestedBy: 'fixture-admin',
  revision: 3,
  updatedAt: '2026-09-14T07:40:00.000Z',
} satisfies PlatformConfigurationApply

function platformConfigurationStatus(): PlatformConfigurationStatusSchema {
  if (activeScene.value.id === 'platform-configuration-unconfigured') {
    return {
      revision: 0,
      appliedState: 'unconfigured',
      defaults: platformConfigurationDefaults,
      desired: null,
      applied: null,
      latestApply: null,
      impact: { runtimePolicyChanged: false, builderPolicyChanged: false, lifecyclePolicyChanged: false },
    }
  }
  return {
    revision: 9,
    appliedState: 'unverified',
    defaults: platformConfigurationDefaults,
    desired: platformConfigurationDesired,
    applied: platformConfigurationApplied,
    appliedAt: '2026-09-10T02:20:00.000Z',
    appliedBundleSha256: platformConfigurationPreviousApply.encryptedBundleSha256,
    latestApply: platformConfigurationLatestApply,
    impact: { runtimePolicyChanged: true, builderPolicyChanged: false, lifecyclePolicyChanged: true },
  }
}

export async function getPlatformConfiguration(): Promise<Result<PlatformConfigurationStatusSchema>> {
  return preview(platformConfigurationStatus())
}
export async function getPlatformConfigurationVersion(options: { path: { revision: number } }): Promise<Result<PlatformConfigurationVersionSchema>> {
  const version = [platformConfigurationApplied, platformConfigurationDesired].find((item) => item.revision === options.path.revision)
  return version ? preview(version) : notFoundResponse('平台配置版本', String(options.path.revision))
}
export async function getPlatformConfigurationApply(options: { path: { applyId: string } }): Promise<Result<PlatformConfigurationApply>> {
  const attempt = [platformConfigurationLatestApply, platformConfigurationPreviousApply].find((item) => item.id === options.path.applyId)
  return attempt ? preview(attempt) : notFoundResponse('平台配置应用尝试', options.path.applyId)
}
export async function updatePlatformConfiguration(_options?: unknown) { return unsupportedResponse('保存平台配置') }
export async function applyPlatformConfiguration(_options?: unknown) { return unsupportedResponse('提交平台配置应用请求') }
