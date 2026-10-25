import { activeScene, fixtureProjectId } from './scenes'
import { agentRunHistory, authoringPublicationStatus, authoringReleases, authoringRubricRules, authoringScene, environmentCandidateView, evaluationCandidateView, reviewPackage, reviewRun, reviewRunId } from './authoring'
import type {
  CreditAccountSchema,
  CreditHoldPageSchema,
  CreditLedgerPageSchema,
  AvailableObjectActionsSchema,
  ObjectAction,
  AccessGrantSchema,
  AgentRunHistoryPageSchema,
  AgentRunTaskProgressSchema,
  ConsoleCapabilityAvailabilitySchema,
  EnvironmentAccessGrantPageSchema,
  EnvironmentEndpointSchema,
  EnvironmentDetailSchema,
  PublicEnvironmentOperation,
  EnvironmentOperationPageSchema,
  EnvironmentOperationSnapshot,
  GpuCatalogEntrySchema,
  ListEnvironmentsData,
  EnvironmentSummary,
  EnvironmentSummaryPageSchema,
  EnvironmentTemplateReleaseViewSchema,
  Notification,
  NotificationPageSchema,
  NotificationUnreadCountSchema,
  ProjectMembershipPageSchema,
  ProjectPageSchema,
  ProjectSchema,
  OrganizationUserPageSchema,
  PlatformImageCatalogSchema,
  PlatformImageCatalogSchemaPlatformImageEntry,
  PlatformImageCatalogViewSchema,
  ProjectAgentCapabilitySchema,
  ProblemDetails,
  ResourceLeaseSchema,
  ResourceLeaseState,
  ResourceChargeSchema,
  ResourceRateSchema,
  ResourceRequestPageSchema,
  ResourceRequestSchema,
  ResourceRequestState,
  SshPublicKeySchema,
  FrozenSubmissionSchema,
  StudentEvaluationResultSchema,
  EvaluationRunTaskProgressSchema,
  WorkloadResources,
} from '../src/generated/contracts/types.gen'

type FixtureSuccess<T> = { data: T; error: undefined }
type FixtureError = { data: undefined; error: unknown }
type FixtureResult<T> = FixtureSuccess<T> | FixtureError
/** Resource list pages (#229): one page and the cursor of the next, null on the last. */
type CursorPage<T> = { items: T[]; nextCursor: string | null }

// Platform configuration (#203) preview facts live in their own module.
export * from './platformConfiguration'

const now = '2026-09-14T08:00:00.000Z'
const later = '2026-09-14T16:00:00.000Z'
const GIB = 1024 ** 3

const project = {
  id: fixtureProjectId,
  name: '量子材料实验室 · 2026 秋季长期项目（演示）',
  description: '用于展示跨课程实验、Work 环境和资源授权的可读项目名称。',
  courseId: 'course-physics-2026',
  ownerActorId: 'fixture-teacher',
  state: 'active',
  revision: 7,
  createdAt: now,
  updatedAt: now,
} satisfies ProjectSchema

const archivedProject = {
  ...project,
  id: 'project-archived-history',
  name: '归档项目 · 高性能计算课程历史记录',
  state: 'archived',
  revision: 4,
} satisfies ProjectSchema

const release = {
  id: 'release-quantum-container',
  version: 3,
  projectId: fixtureProjectId,
  courseId: 'course-physics-2026',
  runtimeKind: 'container',
  name: '量子材料工作环境',
  description: '课程材料的固定工作环境',
  templateFamilyId: 'candidate-quantum-03',
  purpose: 'experiment',
  capabilities: ['terminal', 'applications', 'files', 'agent'],
  publishedAt: '2026-09-12T10:30:00.000Z',
  publishedBy: '林老师',
  agentRunId: 'agent-run-materials-03',
  candidateId: 'candidate-quantum-03',
  candidateRevision: 8,
  approval: {
    actorId: 'fixture-teacher',
    candidateId: 'candidate-quantum-03',
    candidateRevision: 8,
    decidedAt: '2026-09-12T09:45:00.000Z',
    decision: 'approved',
    id: 'approval-quantum-03',
    policyRevision: 2,
    reason: '已完成运行时和收集规则复核。',
    trustRevision: 3,
  },
  artifact: {
    kind: 'container',
    id: 'artifact-quantum-container',
    digest: 'sha256:9a3e7c6d1f2b4a8d3e6f7a1b0c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1',
    repository: 'registry.labweaver.local/quantum-materials',
    build_request_id: 'build-quantum-03',
  },
  submissionManifest: {
    apiVersion: 'evaluation.labweaver.io/v1',
    kind: 'SubmissionManifest',
    name: '量子材料实验提交清单',
    source: 'workspace',
    include: [{ kind: 'directoryTree', path: 'workspace' }],
    required: [{ kind: 'exactFile', path: 'workspace/report.md' }],
    exclude: [{ kind: 'directoryTree', path: 'workspace/.cache' }],
    llmReadable: [{ kind: 'exactFile', path: 'workspace/report.md' }],
    maxFiles: 400,
    maxTotalBytes: 20 * 1024 * 1024,
    followSymlinks: false,
  },
} satisfies EnvironmentTemplateReleaseViewSchema

const vmRelease = {
  ...release,
  id: 'release-quantum-vm',
  version: 1,
  runtimeKind: 'virtual_machine',
  capabilities: ['desktop', 'ssh', 'agent'],
  artifact: {
    kind: 'virtual_machine',
    id: 'artifact-quantum-vm',
    format: 'qcow2',
    base_disk: {
      binding: 'fixture-base-disk',
      capacityBytes: 40 * GIB,
      sourceRegistryDigest: 'sha256:vm-base-disk-fixture',
    },
  },
} satisfies EnvironmentTemplateReleaseViewSchema

/** The pinned container image the manual Work templates below were published from. */
const workTemplateImage = {
  catalogId: '0197f0e0-0000-7000-8000-000000000001',
  kind: 'container',
  binding: 'ubuntu-24.04-v1',
  sourceReference: 'registry.labweaver.local/labweaver-system/ubuntu:24.04',
  resolvedDigest: 'sha256:9a3e7c6d1f2b4a8d3e6f7a1b0c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1',
  mediaType: 'application/vnd.oci.image.manifest.v1+json',
  sizeBytes: 268435456,
  status: 'active',
  trustRevision: 4,
  repinGeneration: 2,
  pinnedAt: now,
  updatedAt: later,
} satisfies PlatformImageCatalogSchemaPlatformImageEntry

/** Manually published Work templates (no Agent run) for the Work launcher preview. */
const manualWorkSpec = {
  apiVersion: 'environment.labweaver.io/v1',
  kind: 'EnvironmentSpec',
  class: 'work',
  name: 'Python 数据分析 Work',
  resources: { cpuMillicores: 2000, memoryBytes: 4 * GIB, storageBytes: 20 * GIB },
  network: { mode: 'deny_all' },
  entries: [{ name: 'app', protocol: 'http', servicePort: 8080 }],
  security: { runAsUser: null, readOnlyRootFilesystem: false, allowPrivilegeEscalation: false, publicExposurePolicy: 'deny', securityProfileBinding: 'restricted-v1' },
  runtime: { kind: 'container', provider_binding: 'fixture-kubernetes', service_port: 8080, terminal: { executable: '/bin/bash', args: [], workingDirectory: '/workspace' } },
  retention: { policyId: '0197f0e0-0000-7000-8000-0000000000c1', policyRevision: 1, class: 'run_evidence', disposition: 'delete', retainUntil: '2027-03-01T00:00:00.000Z' },
} satisfies NonNullable<EnvironmentTemplateReleaseViewSchema['manualApproval']>['spec']

const workRelease = {
  id: 'release-work-python',
  version: 1,
  projectId: fixtureProjectId,
  courseId: 'course-physics-2026',
  runtimeKind: 'container',
  name: 'Python 数据分析 Work',
  description: '预装 Python 3.12、JupyterLab 与常用科学计算库，工作文件保存在 /workspace。',
  templateFamilyId: 'python-analysis',
  purpose: 'work',
  capabilities: ['terminal', 'files', 'applications', 'agent'],
  publishedAt: '2026-09-10T08:00:00.000Z',
  publishedBy: '林老师',
  manualApproval: {
    approvedAt: '2026-09-10T08:00:00.000Z',
    approvedBy: 'fixture-teacher',
    description: '预装 Python 3.12、JupyterLab 与常用科学计算库，工作文件保存在 /workspace。',
    platformImage: workTemplateImage,
    reason: '已核对镜像、端口与资源。',
    spec: manualWorkSpec,
    templateFamilyId: 'python-analysis',
    trustRevision: 4,
  },
  artifact: {
    kind: 'catalog_container',
    id: 'artifact-work-python',
    catalog_id: '0197f0e0-0000-7000-8000-000000000001',
    repin_generation: 2,
    trust_revision: 4,
    repository: 'registry.labweaver.local/labweaver-system/ubuntu',
    digest: 'sha256:9a3e7c6d1f2b4a8d3e6f7a1b0c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1',
  },
} satisfies EnvironmentTemplateReleaseViewSchema

const workReleaseNext = {
  ...workRelease,
  id: 'release-work-python-2',
  version: 2,
  description: '在 v1 基础上增加 R 4.4 与 Quarto。',
  publishedAt: '2026-09-13T08:00:00.000Z',
  manualApproval: { ...workRelease.manualApproval, description: '在 v1 基础上增加 R 4.4 与 Quarto。' },
} satisfies EnvironmentTemplateReleaseViewSchema

const workVmRelease = {
  ...vmRelease,
  id: 'release-work-desktop',
  name: 'Ubuntu 桌面 Work',
  description: '图形桌面与 SSH，适合需要桌面软件的长期工作。',
  templateFamilyId: 'ubuntu-desktop',
  purpose: 'work',
  capabilities: ['desktop', 'ssh', 'agent'],
  submissionManifest: null,
} satisfies EnvironmentTemplateReleaseViewSchema

const resource = {
  cpuMillicores: 2000,
  memoryBytes: 4 * GIB,
  storageBytes: 20 * GIB,
} satisfies WorkloadResources

function makeRequest(id: string, requestKey: string, state: ResourceRequestState, extra: Partial<ResourceRequestSchema> = {}): ResourceRequestSchema {
  return {
    id,
    requestKey,
    projectId: fixtureProjectId,
    requesterId: 'fixture-student',
    courseId: 'course-physics-2026',
    generation: 1,
    revision: 3,
    state,
    requestedDurationSeconds: 8 * 3600,
    requestedResources: { ...resource },
    target: { kind: 'environment', environmentId: 'env-physics-ready', releaseId: release.id, releaseVersion: release.version },
    createdAt: '2026-09-14T07:30:00.000Z',
    updatedAt: '2026-09-14T07:45:00.000Z',
    ...extra,
  }
}

function makeLease(id: string, requestId: string, state: ResourceLeaseState, extra: Partial<ResourceLeaseSchema> = {}): ResourceLeaseSchema {
  return {
    id,
    requestId,
    claimId: `claim-${id}`,
    state,
    revision: 2,
    activeFrom: '2026-09-14T07:50:00.000Z',
    expiresAt: later,
    createdAt: '2026-09-14T07:45:00.000Z',
    updatedAt: now,
    ...extra,
  }
}

function currentEnvironmentState(): EnvironmentDetailSchema['observedState'] | undefined {
  const state = activeScene.value.environmentState
  switch (state) {
    case undefined:
      return undefined
    case 'requested':
    case 'validating':
    case 'building':
    case 'provisioning':
    case 'ready':
    case 'stopping':
    case 'stopped':
    case 'updating':
    case 'expiring':
    case 'deleting':
    case 'deleted':
    case 'failed':
      return state
    default:
      throw new Error(`Fixture 场景包含未识别的环境状态：${state}`)
  }
}

const environmentLabels: Partial<Record<EnvironmentDetailSchema['observedState'], string>> = {
  ready: '量子材料 · 交互实验环境',
  stopped: '量子材料 · 数据分析环境',
  deleted: '量子材料 · 历史实验环境',
  failed: '量子材料 · 置备诊断环境',
}

function environmentDisplayLabel(state: EnvironmentDetailSchema['observedState']): string {
  return environmentLabels[state] ?? '量子材料 · 实验环境'
}

const sceneEnvironmentIds: Partial<Record<string, string>> = {
  'resource-authorized': 'env-physics-ready',
  'resource-reclaim': 'env-physics-ready',
  'workspace-members': 'env-workbench',
  'work-launcher': 'env-workbench',
}

function environmentClassFor(id: string): EnvironmentDetailSchema['class'] {
  return id === 'env-workbench' ? 'work' : 'experiment'
}

function environmentInstance(id: string): EnvironmentDetailSchema {
  const state = currentEnvironmentState() ?? 'ready'
  const environmentClass = environmentClassFor(id)
  const deleted = state === 'deleted'
  const failed = state === 'failed'
  const stopped = state === 'stopped'
  const operation: PublicEnvironmentOperation = {
    id: failed ? 'operation-provision-failed' : `operation-${state}`,
    kind: failed ? 'start' : state === 'stopped' ? 'stop' : state === 'deleted' ? 'delete' : 'start',
    state: failed ? 'failed' : 'succeeded',
    actorId: 'fixture-student',
    attempt: failed ? 2 : 1,
    maxAttempts: 3,
    acceptedAt: '2026-09-14T07:35:00.000Z',
    acceptedRevision: 11,
    deadlineAt: later,
    nextAttemptAt: later,
    preserveMutableDisk: true,
    providerStep: 3,
    traceId: 'trace-fixture-environment',
    diagnosticCode: failed ? 'ENVIRONMENT_PROVISION_FAILED' : null,
    retryFromPhase: failed ? 'provisioning' : null,
  }
  const endpoints: EnvironmentEndpointSchema[] = deleted ? [] : [
    { id: 'endpoint-http', protocol: 'https', health: 'healthy', observedAt: now, revision: 4 },
    { id: 'endpoint-ssh', protocol: 'ssh', health: 'healthy', observedAt: now, revision: 4 },
  ]
  const ready = state === 'ready'
  // Work previews the manually published template; experiments keep the course release.
  const pinned = environmentClass === 'work' ? workRelease : release
  return {
    id,
    displayLabel: environmentDisplayLabel(state),
    projectId: fixtureProjectId,
    courseId: 'course-physics-2026',
    class: environmentClass,
    runtimeKind: 'container',
    releaseId: pinned.id,
    releaseVersion: pinned.version,
    ownerId: 'fixture-student',
    providerBinding: 'fixture-kubernetes',
    capacityBinding: 'fixture-capacity-claim',
    leaseId: 'lease-env-physics',
    approvedResources: { ...resource },
    generation: 1,
    observedGeneration: 1,
    revision: 12,
    desiredState: deleted ? 'deleted' : stopped ? 'stopped' : 'running',
    observedState: state,
    failedPhase: failed ? 'provisioning' : null,
    lastDiagnosticCode: failed ? 'ENVIRONMENT_PROVISION_FAILED' : null,
    eligibilityExpiresAt: later,
    endpoints,
    operation,
    connection: {
      instanceReady: ready,
      state: ready ? 'ready' : deleted || failed ? 'unavailable' : 'preparing',
      observedAt: now,
      reasonCode: ready ? null : `ENVIRONMENT_${state.toUpperCase()}`,
      guestInstructions: ready && environmentClass === 'work'
        ? ['浏览器终端以容器内的非 root 用户运行，工作目录为 /workspace。', 'Web 应用通过平台网关打开，不需要单独的系统密码。']
        : [],
    },
  }
}

function environmentSummary(
  id: string,
  state: EnvironmentSummary['observedState'],
  label: string,
  environmentClass: EnvironmentSummary['class'] = 'experiment',
): EnvironmentSummary {
  const deleted = state === 'deleted'
  return {
    id,
    displayLabel: label,
    projectId: fixtureProjectId,
    courseId: 'course-physics-2026',
    class: environmentClass,
    runtimeKind: 'container',
    releaseId: environmentClass === 'work' ? workRelease.id : release.id,
    releaseVersion: environmentClass === 'work' ? workRelease.version : release.version,
    createdAt: now,
    updatedAt: now,
    eligibilityExpiresAt: later,
    desiredState: deleted ? 'deleted' : state === 'stopped' ? 'stopped' : 'running',
    observedState: state,
    revision: 12,
    lastChangedStreamSequence: 'fixture-42',
    owner: { relation: 'self_owned', displayLabel: '周同学' },
    access: { state: deleted ? 'ineligible' : 'eligible', activeGrantCount: 0, healthyEndpointCount: deleted ? 0 : 2, reasonCode: deleted ? 'ENVIRONMENT_DELETED' : null },
    currentOperation: state === 'failed' ? {
      operationId: 'operation-provision-failed',
      environmentId: id,
      kind: 'start',
      state: 'failed',
      acceptedAt: now,
      acceptedRevision: 11,
      attempt: 2,
      maxAttempts: 3,
      cancelEligible: false,
      retryEligible: true,
      deadlineAt: later,
      diagnosticCode: 'ENVIRONMENT_PROVISION_FAILED',
      traceId: 'trace-fixture-failed',
    } : null,
  }
}

function projectsForScene(): ProjectSchema[] {
  return activeScene.value.id === 'teacher-project-empty' ? [] : [project, archivedProject]
}

/** Scenes that create or open Work and so need Work-purpose releases. */
const WORK_RELEASE_SCENES: Partial<Record<string, true>> = {
  'work-launcher': true,
  'work-upgrade': true,
  'resource-processing': true,
  'resource-authorized': true,
  'resource-reclaim': true,
}

function releasesForScene(): EnvironmentTemplateReleaseViewSchema[] {
  if (authoringScene()) return authoringReleases()
  if (['teacher-template-empty', 'resource-empty', 'work-launcher-empty'].includes(activeScene.value.id)) return []
  return WORK_RELEASE_SCENES[activeScene.value.id]
    ? [workRelease, workReleaseNext, workVmRelease, release]
    : [release, vmRelease]
}

function projectRequests(): ResourceRequestSchema[] {
  switch (activeScene.value.id) {
    case 'resource-processing':
      return [makeRequest('request-reviewing', 'work-20260914-review', 'reviewing'), makeRequest('request-allocating', 'work-20260914-allocating', 'allocating')]
    case 'resource-authorized':
    case 'resource-reclaim':
      return [makeRequest('request-authorized', 'work-20260914-authorized', 'active')]
    case 'home-in-progress':
      return [makeRequest('request-reviewing', 'work-20260914-review', 'reviewing')]
    default:
      return []
  }
}

function approvalRequests(): ResourceRequestSchema[] {
  if (activeScene.value.id !== 'approval-filter-empty') return []
  return [
    makeRequest('request-approval-task', 'task-20260914-physics', 'reviewing', {
      target: { kind: 'task', taskRunId: 'task-run-physics-001' },
      projectId: fixtureProjectId,
    }),
    makeRequest('request-approval-active', 'work-20260913-active', 'active', {
      target: { kind: 'environment', environmentId: 'env-physics-ready', releaseId: release.id, releaseVersion: release.version },
    }),
  ]
}

function projectLeases(): ResourceLeaseSchema[] {
  return activeScene.value.id === 'resource-authorized' || activeScene.value.id === 'resource-reclaim'
    ? [makeLease('lease-resource-authorized', 'request-authorized', 'active')]
    : []
}

function approvalLeases(): ResourceLeaseSchema[] {
  return activeScene.value.id === 'approval-filter-empty'
    ? [makeLease('lease-admin-active', 'request-approval-active', 'active')]
    : []
}

/**
 * Administrator platform image catalog preview.
 *
 * The catalog is Agent-owned and Control adds the release impact hint, so the
 * fixture renders exactly the gateway projection the page consumes.
 */
const platformImageCatalog = {
  entries: [
    {
      catalogId: '0197f0e0-0000-7000-8000-000000000001',
      kind: 'container',
      binding: 'ubuntu-24.04-v1',
      sourceReference: 'registry.labweaver.local/labweaver-system/ubuntu:24.04',
      resolvedDigest: 'sha256:9a3e7c6d1f2b4a8d3e6f7a1b0c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1',
      mediaType: 'application/vnd.oci.image.manifest.v1+json',
      sizeBytes: 268435456,
      status: 'active',
      trustRevision: 4,
      repinGeneration: 2,
      pinnedAt: now,
      updatedAt: later,
      releaseReferenceCount: 2,
    },
    {
      catalogId: '0197f0e0-0000-7000-8000-000000000002',
      kind: 'virtual_machine',
      binding: 'ubuntu-24.04-vm-v1',
      sourceReference: 'registry.labweaver.local/labweaver-system/ubuntu-vm:24.04',
      resolvedDigest: 'sha256:4b1d2f8a6c0e9d7b5a3f1e8c6d4b2a0f9e7c5d3b1a8f6e4c2d0b9a7f5e3c1d0b',
      mediaType: 'application/vnd.oci.image.manifest.v1+json',
      sizeBytes: 6442450944,
      capacityBytes: 10737418240,
      diskSha256: 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
      format: 'qcow2',
      status: 'active',
      trustRevision: 2,
      repinGeneration: 1,
      pinnedAt: now,
      updatedAt: now,
      releaseReferenceCount: 1,
    },
    {
      catalogId: '0197f0e0-0000-7000-8000-000000000003',
      kind: 'container',
      binding: 'alpine-3.20-v1',
      sourceReference: 'registry.labweaver.local/labweaver-system/alpine:3.20',
      resolvedDigest: 'sha256:7c5e3a1b9d8f6e4c2a0b8d7f5e3c1a9b7d6f4e2c0a8b6d5f3e1c9a7b5d4f2e0c',
      mediaType: 'application/vnd.oci.image.manifest.v1+json',
      sizeBytes: 8388608,
      status: 'disabled',
      trustRevision: 1,
      repinGeneration: 1,
      pinnedAt: now,
      updatedAt: later,
      releaseReferenceCount: 0,
    },
  ],
} satisfies PlatformImageCatalogViewSchema

/**
 * Administrator GPU catalog preview.
 *
 * Resource Service keeps every revision; the fixture shows one active class
 * plus the revision it retired so the page renders both states.
 */
export const gpuCatalogFixture = [
  {
    id: '0197f0e0-0000-7000-8000-0000000000a1',
    class: 'nvidia-a10',
    mode: 'exclusive',
    providerBinding: 'fixture-kubernetes',
    capacityUnits: 4,
    allocationBinding: 'fixture-nvidia-a10',
    revision: 2,
    active: true,
  },
  {
    id: '0197f0e0-0000-7000-8000-0000000000a2',
    class: 'nvidia-a10',
    mode: 'exclusive',
    providerBinding: 'fixture-kubernetes',
    capacityUnits: 2,
    allocationBinding: 'fixture-nvidia-a10',
    revision: 1,
    active: false,
  },
] satisfies GpuCatalogEntrySchema[]

function fixtureResponse<T>(data: T): Promise<FixtureSuccess<T>> {
  return Promise.resolve({ data, error: undefined })
}

function fixtureProblem(
  diagnosticCode: 'FIXTURE_OPERATION_UNSUPPORTED' | 'FIXTURE_RESOURCE_NOT_FOUND',
  detail: string,
  status: 404 | 501,
): ProblemDetails {
  return {
    type: 'about:blank',
    title: diagnosticCode === 'FIXTURE_OPERATION_UNSUPPORTED' ? '预览操作未覆盖' : '预览资源不存在',
    status,
    detail,
    instance: '/fixture-preview',
    requestId: 'fixture-' + diagnosticCode.toLowerCase(),
    diagnosticCode,
    retryable: false,
  }
}

function unsupportedResponse(operation: string): Promise<FixtureError> {
  return Promise.resolve({
    data: undefined,
    error: fixtureProblem(
      'FIXTURE_OPERATION_UNSUPPORTED',
      'Fixture 预览未覆盖 ' + operation + '，不能执行真实写操作。',
      501,
    ),
  })
}

function notFoundResponse(resourceName: string, id: string | undefined): Promise<FixtureError> {
  const suffix = id ? '：' + id : '（缺少标识）'
  return Promise.resolve({
    data: undefined,
    error: fixtureProblem('FIXTURE_RESOURCE_NOT_FOUND', 'Fixture 预览未覆盖' + resourceName + suffix + '。', 404),
  })
}

interface FixtureOptions {
  path?: Record<string, string | undefined>
  query?: Partial<ListEnvironmentsData['query']> & {
    query?: string
    page?: number
    pageSize?: number
    cursor?: string
    limit?: number
  }
}

export async function listProjects(_options?: FixtureOptions): Promise<FixtureResult<ProjectPageSchema>> {
  return fixtureResponse({ items: projectsForScene(), nextCursor: null })
}

export async function listOrganizationUsers(_options?: FixtureOptions): Promise<FixtureResult<OrganizationUserPageSchema>> {
  return unsupportedResponse('组织账号目录搜索')
}

export async function listProjectAgentRuns(_options?: FixtureOptions): Promise<FixtureResult<AgentRunHistoryPageSchema>> {
  if (activeScene.value.id === 'teacher-review-empty') return fixtureResponse({ items: [], nextCursor: null })
  if (authoringScene()) return fixtureResponse(agentRunHistory)
  if (activeScene.value.id !== 'home-in-progress') {
    return fixtureResponse({ items: [], nextCursor: null })
  }
  return fixtureResponse({
    items: [
      { id: 'agent-run-materials-04', projectId: fixtureProjectId, courseId: 'course-physics-2026', purpose: { kind: 'authoring', environmentClass: 'experiment' }, state: 'awaiting_approval', createdAt: now, updatedAt: later },
      { id: 'agent-run-work-config-02', projectId: fixtureProjectId, courseId: 'course-physics-2026', purpose: { kind: 'work_configuration', actorId: 'fixture-teacher', environmentId: 'env-workbench', environmentRevision: 3, runtimeKind: 'container' }, state: 'running', createdAt: now, updatedAt: now },
      { id: 'agent-run-materials-03', projectId: fixtureProjectId, courseId: 'course-physics-2026', purpose: { kind: 'authoring', environmentClass: 'experiment' }, state: 'succeeded', createdAt: now, updatedAt: now },
    ],
    nextCursor: null,
  })
}

/** Home scene: the authoring run waits for review; Work configuration retried once and waits for resource approval. */
export async function getProjectAgentTaskProgress(options?: FixtureOptions): Promise<FixtureResult<AgentRunTaskProgressSchema>> {
  const runId = options?.path?.runId
  const base = { projectId: fixtureProjectId, page: 1, pageSize: 1, hasMore: false }
  if (activeScene.value.id === 'home-in-progress' && options?.path?.projectId === fixtureProjectId) {
    if (runId === 'agent-run-materials-04') {
      return fixtureResponse({ ...base, runId, revision: 9, state: 'awaiting_approval', history: [], current: [
        { track: 'environment', progress: { attempt: 1, executionGeneration: 1, taskRunId: 'task-run-env-1', phase: 'succeeded', nextSteps: [] } },
        { track: 'evaluation', progress: { attempt: 1, executionGeneration: 1, taskRunId: 'task-run-eval-1', phase: 'awaiting_review', nextSteps: ['completeProjectAuthoringApproval'] } },
      ] })
    }
    if (runId === 'agent-run-work-config-02') {
      return fixtureResponse({ ...base, runId, revision: 4, state: 'running', hasMore: true, current: [
        { track: 'work_configuration', progress: { attempt: 2, taskRunId: 'task-run-work-2', phase: 'waiting_resource_approval', resourceRequestId: 'resource-request-work-2', resourceState: 'reviewing', nextSteps: [] } },
      ], history: [
        { track: 'work_configuration', progress: { attempt: 1, executionGeneration: 1, taskRunId: 'task-run-work-1', phase: 'failed', diagnosticCode: 'LW_TASK_PLATFORM_FAILED', nextSteps: [] } },
      ] })
    }
  }
  return notFoundResponse('读取任务进度', runId)
}

export async function getProjectAgentRun(options?: FixtureOptions) {
  if (authoringScene() && options?.path?.runId === reviewRunId) return fixtureResponse(reviewRun)
  return unsupportedResponse('读取项目任务')
}
export async function getProjectEnvironmentCandidate(_options?: FixtureOptions) {
  return authoringScene() ? fixtureResponse(environmentCandidateView) : unsupportedResponse('读取环境候选')
}
export async function getProjectEvaluationCandidate(_options?: FixtureOptions) {
  return authoringScene() ? fixtureResponse(evaluationCandidateView) : unsupportedResponse('读取评测候选')
}
export async function getProjectProblemPackage(_options?: FixtureOptions) {
  return authoringScene() ? fixtureResponse(reviewPackage) : unsupportedResponse('读取材料包')
}
export async function getProjectAuthoringApproval(options?: FixtureOptions) {
  const status = authoringPublicationStatus(options?.path?.approvalId)
  return status ? fixtureResponse(status) : unsupportedResponse('读取实验包批准')
}
export async function completeProjectAuthoringApproval() { return unsupportedResponse('提交实验包批准') }
export async function getProjectCandidateBuild() { return unsupportedResponse('读取候选构建任务') }
export async function cancelProjectCandidateBuild() { return unsupportedResponse('取消候选构建任务') }
export async function getProjectRubricModelInputRules(_options?: FixtureOptions) {
  const rules = authoringRubricRules()
  return rules ? fixtureResponse(rules) : unsupportedResponse('读取评分规则的模型使用设置')
}
export async function maintainProjectRubricModelInputRule(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('修改评分规则的模型使用设置') }

export async function getProject(options?: FixtureOptions): Promise<FixtureResult<ProjectSchema>> {
  const projectId = options?.path?.projectId
  if (!projectId) return notFoundResponse('读取项目', projectId)
  const item = projectsForScene().find((candidate) => candidate.id === projectId)
  return item ? fixtureResponse(item) : notFoundResponse('读取项目', projectId)
}

function knownProject(projectId: string | undefined): ProjectSchema | undefined {
  if (!projectId) return undefined
  return [project, archivedProject].find((candidate) => candidate.id === projectId)
}

export async function listProjectMemberships(options?: FixtureOptions): Promise<FixtureResult<ProjectMembershipPageSchema>> {
  const projectId = options?.path?.projectId
  const selectedProject = knownProject(projectId)
  if (!selectedProject) return notFoundResponse('读取项目成员', projectId)
  if (selectedProject.state === 'archived') return fixtureResponse({ items: [], nextCursor: null })
  return fixtureResponse({
    items: activeScene.value.id === 'workspace-members' || activeScene.value.id === 'workspace-viewer' ? [
      { actorId: '周同学', projectId: fixtureProjectId, role: 'student', objectRole: 'operator', state: 'active', revision: 2, courseId: 'course-physics-2026', expiresAt: null },
      { actorId: '助教·陈老师', projectId: fixtureProjectId, role: 'teacher', objectRole: 'manager', state: 'active', revision: 1, courseId: 'course-physics-2026', expiresAt: null },
    ] : [],
    nextCursor: null,
  })
}

const OWNER_ACTIONS: ObjectAction[] = ['browse', 'archive', 'edit', 'start', 'stop', 'configure_agent', 'grant_access', 'manage_members', 'view_costs', 'request_runtime_permissions']
const EDITOR_ACTIONS: ObjectAction[] = ['browse', 'edit', 'start', 'stop', 'configure_agent', 'grant_access', 'view_costs', 'request_runtime_permissions']

/**
 * Preview of the current actor's object actions. Teacher and administrator
 * scenes preview the project owner, student scenes an editor of their own
 * experiment project, and the `*-viewer` scenes a read-only member, so each
 * permission state of the console can be inspected.
 */
export async function getProjectAvailableActions(options?: FixtureOptions): Promise<FixtureResult<AvailableObjectActionsSchema>> {
  const projectId = options?.path?.projectId
  if (!knownProject(projectId) || !projectId) return notFoundResponse('读取项目可用动作', projectId)
  const scene = activeScene.value
  const actions = scene.id.endsWith('-viewer') ? ['browse' as const] : scene.role === 'student' ? EDITOR_ACTIONS : OWNER_ACTIONS
  return fixtureResponse({ projectId, actorId: `fixture-${scene.role}`, membershipRevision: 1, validUntil: '2099-01-01T00:00:00.000Z', actions })
}

export async function listEnvironments(options?: FixtureOptions): Promise<FixtureResult<EnvironmentSummaryPageSchema>> {
  const projectId = options?.query?.projectId
  const selectedProject = knownProject(projectId)
  if (!selectedProject) return notFoundResponse('读取环境（项目）', projectId)
  if (selectedProject.state === 'archived') {
    return fixtureResponse({ items: [], nextCursor: null, snapshotAt: now, snapshotSequence: 'fixture-42' })
  }

  if (activeScene.value.id === 'home-in-progress') {
    return fixtureResponse({
      items: [
        environmentSummary('env-physics-ready', 'ready', '量子材料 · 交互实验环境'),
        environmentSummary('env-workbench', 'ready', 'Work · 分析工作空间', 'work'),
        environmentSummary('env-physics-failed', 'failed', '量子材料 · GPU 训练环境'),
      ],
      nextCursor: null,
      snapshotAt: now,
      snapshotSequence: 'fixture-42',
    })
  }
  const environmentId = sceneEnvironmentIds[activeScene.value.id] ?? activeScene.value.environmentId
  const state = currentEnvironmentState()
  const environmentClass = environmentId ? environmentClassFor(environmentId) : undefined
  const environmentState = environmentId ? (state ?? 'ready') : undefined
  const matchesClass = !options?.query?.class || options.query.class === environmentClass
  const items = environmentId && environmentState && matchesClass
    ? [environmentSummary(environmentId, environmentState, environmentClass === 'work' ? 'Work · 分析工作空间' : environmentDisplayLabel(environmentState), environmentClass)]
    : []
  return fixtureResponse({ items, nextCursor: null, snapshotAt: now, snapshotSequence: 'fixture-42' })
}

export async function listEnvironmentTemplateReleases(options?: FixtureOptions): Promise<FixtureResult<{
  items: EnvironmentTemplateReleaseViewSchema[]
  nextCursor?: string | null
}>> {
  const projectId = options?.path?.projectId
  if (projectId && projectId !== fixtureProjectId) return notFoundResponse('读取项目环境模板', projectId)
  return fixtureResponse({ items: releasesForScene(), nextCursor: null })
}

function currentEnvironmentId(): string | undefined {
  return sceneEnvironmentIds[activeScene.value.id] ?? activeScene.value.environmentId
}

export async function getEnvironment(options?: FixtureOptions): Promise<FixtureResult<EnvironmentDetailSchema>> {
  const environmentId = options?.path?.environmentId
  if (!environmentId || environmentId !== currentEnvironmentId()) return notFoundResponse('读取环境', environmentId)
  return fixtureResponse(environmentInstance(environmentId))
}

export async function listEnvironmentEndpoints(options?: FixtureOptions): Promise<FixtureResult<{ items: EnvironmentEndpointSchema[] }>> {
  const environmentId = options?.path?.environmentId
  if (!environmentId || environmentId !== currentEnvironmentId()) return notFoundResponse('读取环境入口', environmentId)
  const instance = environmentInstance(environmentId)
  return fixtureResponse({ items: instance.endpoints })
}

export async function listEnvironmentAccessGrants(options?: FixtureOptions): Promise<FixtureResult<EnvironmentAccessGrantPageSchema>> {
  const environmentId = options?.path?.environmentId
  if (environmentId && environmentId !== currentEnvironmentId()) return notFoundResponse('读取环境访问授权', environmentId)
  return fixtureResponse({ items: [], nextCursor: null, snapshotAt: now, snapshotSequence: 'fixture-42' })
}

export async function getAccessGrant(options?: FixtureOptions): Promise<FixtureResult<AccessGrantSchema>> {
  const grantId = options?.path?.grantId
  if (!grantId || grantId !== 'grant-fixture') return notFoundResponse('读取访问授权', grantId)
  return fixtureResponse({
    id: grantId,
    environmentId: activeScene.value.environmentId ?? 'env-physics-ready',
    environmentRevision: 12,
    projectId: fixtureProjectId,
    actorId: 'fixture-student',
    state: 'active',
    revision: 1,
    issuedAt: now,
    expiresAt: later,
    endpointGrants: [],
  })
}

export async function createAccessGrant(): Promise<FixtureError> { return unsupportedResponse('签发访问授权') }

export async function listEnvironmentOperations(options?: FixtureOptions): Promise<FixtureResult<EnvironmentOperationPageSchema>> {
  const environmentId = options?.path?.environmentId
  if (environmentId && environmentId !== currentEnvironmentId()) return notFoundResponse('读取环境操作', environmentId)
  const state = currentEnvironmentState()
  const items: EnvironmentOperationSnapshot[] = state === 'failed' ? [{
    operationId: 'operation-provision-failed',
    environmentId: activeScene.value.environmentId ?? 'env-physics-failed',
    kind: 'start',
    state: 'failed',
    acceptedAt: now,
    acceptedRevision: 11,
    attempt: 2,
    maxAttempts: 3,
    cancelEligible: false,
    retryEligible: true,
    deadlineAt: later,
    diagnosticCode: 'ENVIRONMENT_PROVISION_FAILED',
    traceId: 'trace-fixture-failed',
  }] : []
  return fixtureResponse({ items, nextCursor: null, snapshotAt: now, snapshotSequence: 'fixture-42' })
}
export async function listProjectResourceRequests(options?: FixtureOptions): Promise<FixtureResult<ResourceRequestPageSchema>> {
  const projectId = options?.path?.projectId
  if (projectId && projectId !== fixtureProjectId) return notFoundResponse('读取项目资源申请', projectId)
  return fixtureResponse({ items: projectRequests(), nextCursor: null })
}
export async function listProjectResourceLeases(options?: FixtureOptions): Promise<FixtureResult<CursorPage<ResourceLeaseSchema>>> {
  const projectId = options?.path?.projectId
  if (projectId && projectId !== fixtureProjectId) return notFoundResponse('读取项目资源授权', projectId)
  return fixtureResponse({ items: projectLeases(), nextCursor: null })
}

export async function getResourceRequest(options?: FixtureOptions): Promise<FixtureResult<ResourceRequestSchema>> {
  const requestId = options?.path?.requestId
  if (!requestId) return notFoundResponse('读取资源申请', requestId)
  const item = [...projectRequests(), ...approvalRequests()].find((candidate) => candidate.id === requestId)
  return item ? fixtureResponse(item) : notFoundResponse('读取资源申请', requestId)
}

export async function getResourceLease(options?: FixtureOptions): Promise<FixtureResult<ResourceLeaseSchema>> {
  const leaseId = options?.path?.leaseId
  if (!leaseId) return notFoundResponse('读取资源授权', leaseId)
  const item = [...projectLeases(), ...approvalLeases()].find((candidate) => candidate.id === leaseId)
  return item ? fixtureResponse(item) : notFoundResponse('读取资源授权', leaseId)
}

export async function listResourceRequests(): Promise<FixtureResult<ResourceRequestPageSchema>> { return fixtureResponse({ items: approvalRequests(), nextCursor: null }) }
export async function listResourceLeases(): Promise<FixtureResult<CursorPage<ResourceLeaseSchema>>> { return fixtureResponse({ items: approvalLeases(), nextCursor: null }) }
export async function listSshPublicKeys(): Promise<FixtureResult<{ items: SshPublicKeySchema[]; nextCursor?: string | null }>> {
  return fixtureResponse({ items: [], nextCursor: null })
}

export async function listConsoleCapabilities(options?: FixtureOptions): Promise<FixtureResult<ConsoleCapabilityAvailabilitySchema>> {
  const grantId = options?.path?.grantId
  if (grantId && grantId !== 'grant-fixture') return notFoundResponse('读取控制台能力', grantId)
  return fixtureResponse({
    accessGrantId: 'grant-fixture',
    accessGrantRevision: 1,
    environmentClass: 'experiment',
    environmentId: activeScene.value.environmentId ?? 'env-physics-ready',
    environmentRevision: 12,
    expiresAt: later,
    kinds: ['xterm'],
    projectId: fixtureProjectId,
  })
}
export async function issueConsoleCapability(): Promise<FixtureError> { return unsupportedResponse('签发控制台能力') }

export async function createProject() { return unsupportedResponse('创建项目') }
export async function updateProject() { return unsupportedResponse('更新项目') }
export async function archiveProject() { return unsupportedResponse('归档项目') }
export async function addProjectMembership() { return unsupportedResponse('添加项目成员') }
export async function removeProjectMembership() { return unsupportedResponse('移除项目成员') }
export async function createEnvironment() { return unsupportedResponse('创建环境') }
export async function startEnvironment() { return unsupportedResponse('启动环境') }
export async function stopEnvironment() { return unsupportedResponse('停止环境') }
export async function restartEnvironment() { return unsupportedResponse('重启环境') }
export async function deleteEnvironment() { return unsupportedResponse('删除环境') }
export async function cancelEnvironmentOperation() { return unsupportedResponse('取消环境操作') }
export async function retryEnvironment() { return unsupportedResponse('重试环境操作') }
export async function revokeAccessGrant() { return unsupportedResponse('撤销访问授权') }
export async function createResourceRequest() { return unsupportedResponse('创建资源申请') }
export async function cancelResourceRequest() { return unsupportedResponse('取消资源申请') }
export async function renewResourceLease() { return unsupportedResponse('续期资源使用授权') }
export async function revokeResourceLease() { return unsupportedResponse('回收资源使用授权') }
export async function approveResourceRequest() { return unsupportedResponse('批准资源申请') }
export async function rejectResourceRequest() { return unsupportedResponse('拒绝资源申请') }
export async function retryResourceRequest() { return unsupportedResponse('重试资源申请') }
export async function resizeAndApproveResourceRequest() { return unsupportedResponse('调整并批准资源申请') }
export async function createSshPublicKey() { return unsupportedResponse('添加 SSH 公钥') }
export async function deleteSshPublicKey() { return unsupportedResponse('删除 SSH 公钥') }
export async function listEnvironmentAccessGrantsForFixture() { return fixtureResponse({ items: [] }) }
export async function freezeSubmission() { return unsupportedResponse('冻结提交') }
const EVALUATION_SCENES = new Set(['student-results', 'student-result-score', 'student-result-zero', 'student-result-platform', 'student-result-running'])

function frozenSubmissionFixture(id: string, frozenAt: string): FrozenSubmissionSchema {
  return {
    id,
    projectId: fixtureProjectId,
    courseId: 'course-physics-2026',
    actorId: 'fixture-student',
    agentRunId: 'agent-run-materials-03',
    attempt: 1,
    contentSha256: 'f'.repeat(64),
    environment: { environmentId: 'env-physics-ready', environmentRevision: 12, releaseId: release.id, releaseVersion: release.version, runtimeKind: 'container' },
    files: [],
    frozenAt,
    manifestRevision: 1,
    object: { bucket: 'submissions', key: `${id}.tar`, objectVersion: 'v1', mediaType: 'application/x-tar', sizeBytes: 20480, sha256: 'f'.repeat(64) },
    retention: { class: 'submission', disposition: 'delete', policyId: 'policy-submission', policyRevision: 1, retainUntil: later },
    systemFacts: {},
  } as unknown as FrozenSubmissionSchema
}

const frozenSubmissionFixtures: Record<string, string> = {
  'submission-score': '2026-09-14T09:10:00.000Z',
  'submission-zero': '2026-09-14T08:40:00.000Z',
  'submission-platform': '2026-09-14T08:20:00.000Z',
  'submission-cancelled': '2026-09-13T15:00:00.000Z',
  'submission-pending': '2026-09-14T09:30:00.000Z',
}

function evaluationResultFixtures(): StudentEvaluationResultSchema[] {
  const base = { projectId: fixtureProjectId, courseId: 'course-physics-2026', releaseId: 'evaluation-release-quantum', revision: 3, maxScore: 100, createdAt: now, updatedAt: later }
  return [
    {
      ...base, runId: 'evaluation-run-score', frozenSubmissionId: 'submission-score', state: 'succeeded', awardedScore: 86, completedAt: '2026-09-14T09:14:00.000Z',
      steps: [
        { position: 0, role: 'gate', state: 'succeeded', maxScore: 0, awardedScore: 0, diagnosticCode: 'LW_OJ_ACCEPTED' },
        { position: 1, role: 'score', state: 'succeeded', maxScore: 100, awardedScore: 86, diagnosticCode: 'LW_OJ_WRONG_ANSWER' },
        {
          position: 2, role: 'advisory', state: 'succeeded', maxScore: 0,
          review: {
            schema_version: 'goal-review/v1', assessment: 'partially_met', confidence: 0.72, requires_teacher_attention: false,
            findings: [
              { criterion: '能量本征值求解', result: 'met', suggestion: '数值求解与解析结果一致。', evidence: [{ path: 'solver/eigen.py', start_line: 12, end_line: 40 }] },
              { criterion: '边界条件说明', result: 'partial', suggestion: '补充周期性边界条件的推导过程。', evidence: [{ path: 'report.md', start_line: 3, end_line: 18 }] },
            ],
          },
        },
      ],
    },
    {
      ...base, runId: 'evaluation-run-zero', frozenSubmissionId: 'submission-zero', state: 'succeeded', awardedScore: 0, completedAt: '2026-09-14T08:43:00.000Z',
      steps: [
        { position: 0, role: 'gate', state: 'failed', maxScore: 0, awardedScore: 0, diagnosticCode: 'LW_OJ_COMPILE_ERROR' },
        { position: 1, role: 'score', state: 'skipped', maxScore: 100, awardedScore: 0 },
      ],
    },
    {
      ...base, runId: 'evaluation-run-platform', frozenSubmissionId: 'submission-platform', state: 'failed', awardedScore: null, completedAt: '2026-09-14T08:31:00.000Z', diagnosticCode: 'LW_OJ_INFRASTRUCTURE_ERROR',
      steps: [
        { position: 0, role: 'gate', state: 'succeeded', maxScore: 0, awardedScore: 0, diagnosticCode: 'LW_OJ_ACCEPTED' },
        { position: 1, role: 'score', state: 'failed', maxScore: 100, diagnosticCode: 'LW_OJ_INFRASTRUCTURE_ERROR' },
      ],
    },
    {
      ...base, runId: 'evaluation-run-cancelled', frozenSubmissionId: 'submission-cancelled', state: 'cancelled', awardedScore: null, completedAt: '2026-09-13T15:05:00.000Z', diagnosticCode: 'LW_EVALUATION_CANCELLED',
      steps: [{ position: 0, role: 'gate', state: 'cancelled', maxScore: 0 }],
    },
  ] as StudentEvaluationResultSchema[]
}

export async function getFrozenSubmission(options?: FixtureOptions): Promise<FixtureResult<FrozenSubmissionSchema>> {
  const submissionId = options?.path?.submissionId
  const frozenAt = submissionId ? frozenSubmissionFixtures[submissionId] : undefined
  if (!EVALUATION_SCENES.has(activeScene.value.id) || !submissionId || !frozenAt || options?.path?.projectId !== fixtureProjectId) {
    return notFoundResponse('读取冻结提交', submissionId)
  }
  return fixtureResponse(frozenSubmissionFixture(submissionId, frozenAt))
}

export async function listPlatformImages(): Promise<FixtureResult<PlatformImageCatalogViewSchema>> {
  return fixtureResponse(platformImageCatalog)
}
export async function registerPlatformImage() { return unsupportedResponse('注册平台镜像') }
export async function repinPlatformImage() { return unsupportedResponse('重新固定平台镜像') }
export async function disablePlatformImage() { return unsupportedResponse('停用平台镜像') }
export async function createPlatformImageUpload() { return unsupportedResponse('创建平台镜像上传会话') }
export async function getPlatformImageUpload() { return unsupportedResponse('读取平台镜像上传会话') }
export async function cancelPlatformImageUpload() { return unsupportedResponse('取消平台镜像上传会话') }
export async function completePlatformImageUpload() { return unsupportedResponse('完成平台镜像导入') }

/** Active pinned images a project reviewer may publish a manual Work template from. */
export async function listProjectTemplateImages(options?: FixtureOptions): Promise<FixtureResult<PlatformImageCatalogSchema>> {
  const projectId = options?.path?.projectId
  if (projectId !== fixtureProjectId) return notFoundResponse('读取项目平台镜像', projectId)
  return fixtureResponse({
    entries: platformImageCatalog.entries
      .filter((entry) => entry.status === 'active')
      .map(({ releaseReferenceCount: _count, ...entry }) => entry),
  })
}

/** Work launcher scenes model a project with no model configured; Agent features are off, Work is not. */
export async function getProjectAgentCapability(options?: FixtureOptions): Promise<FixtureResult<ProjectAgentCapabilitySchema>> {
  const projectId = options?.path?.projectId
  if (projectId !== fixtureProjectId) return notFoundResponse('读取项目 Agent 配置', projectId)
  if (activeScene.value.id.startsWith('work-')) return fixtureResponse({ status: 'unconfigured', reasonCode: 'LW_AGENT_MODEL_NOT_CONFIGURED' })
  return unsupportedResponse('读取项目 Agent 配置')
}

export async function upgradeEnvironmentTemplate(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('升级 Work 模板') }

export async function listResourceGpuCatalog(): Promise<FixtureResult<CursorPage<GpuCatalogEntrySchema>>> { return fixtureResponse({ items: gpuCatalogFixture, nextCursor: null }) }

const resourceRatesFixture = [
  { id: 'rate-cpu-fixture', revision: 1, unit: 'cpu_millicore_second', unitQuantity: 1000, gpuClass: null, gpuMode: null, unitPrice: { currency: 'USD', amount: '0.000010' }, effectiveFrom: '2026-01-01T00:00:00.000Z', effectiveUntil: null },
  { id: 'rate-gpu-fixture', revision: 1, unit: 'gpu_unit_second', unitQuantity: 1, gpuClass: 'nvidia-a10', gpuMode: 'exclusive', unitPrice: { currency: 'USD', amount: '0.000800' }, effectiveFrom: '2026-01-01T00:00:00.000Z', effectiveUntil: null },
] satisfies ResourceRateSchema[]

export async function listResourceRates(_options?: FixtureOptions): Promise<FixtureResult<CursorPage<ResourceRateSchema>>> {
  return fixtureResponse({ items: resourceRatesFixture, nextCursor: null })
}

const resourceChargesFixture = [{
  id: 'charge-physics-september',
  usageRecordId: 'usage-physics-september',
  projectId: fixtureProjectId,
  lines: [
    { rateId: 'rate-cpu-fixture', rateRevision: 1, unit: 'cpu_millicore_second', quantity: 720000000000, unitQuantity: 1000, unitPrice: { currency: 'USD', amount: '0.000010' }, amount: { currency: 'USD', amount: '7200.000000' } },
    { rateId: 'rate-gpu-fixture', rateRevision: 1, unit: 'gpu_unit_second', quantity: 216000, unitQuantity: 1, unitPrice: { currency: 'USD', amount: '0.000800' }, amount: { currency: 'USD', amount: '172.800000' } },
  ],
  total: { currency: 'USD', amount: '7372.800000' },
  settlement: 'pending',
  createdAt: now,
}] satisfies ResourceChargeSchema[]

export async function listProjectResourceCharges(options?: FixtureOptions): Promise<FixtureResult<CursorPage<ResourceChargeSchema>>> {
  const projectId = options?.path?.projectId
  if (projectId !== fixtureProjectId) return notFoundResponse('读取项目费用明细', projectId)
  return fixtureResponse({ items: activeScene.value.id === 'budget-large' ? resourceChargesFixture : [], nextCursor: null })
}
export async function createResourceGpuCatalogEntry() { return unsupportedResponse('创建 GPU 目录项') }
export async function createResourceRate() { return unsupportedResponse('创建资源费率') }
export async function endResourceRate() { return unsupportedResponse('结束资源费率') }

// Preview-only boundary: SDK operations the gallery does not cover fail
// explicitly instead of crashing the module graph or faking a success.
export async function getActiveProjectLlmPolicy(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('读取项目 AI 设置') }
export async function withdrawEnvironmentTemplateRelease(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('撤回环境模板版本') }
export async function getOwnProjectEvaluationResult(options?: FixtureOptions): Promise<FixtureResult<StudentEvaluationResultSchema>> {
  const runId = options?.path?.runId
  const inScene = EVALUATION_SCENES.has(activeScene.value.id) && options?.path?.projectId === fixtureProjectId
  // A run without a terminal result answers like the real service: 404 / LW_EVALUATION_RUN_NOT_FOUND.
  if (inScene && runId === runningEvaluationRunId) return evaluationRunNotFound(runId)
  const result = inScene ? evaluationResultFixtures().find((item) => item.runId === runId) : undefined
  return result ? fixtureResponse(result) : notFoundResponse('读取评测结果', runId)
}
export async function listOwnProjectEvaluationResults(options?: FixtureOptions): Promise<FixtureResult<{ items: StudentEvaluationResultSchema[]; nextCursor: null }>> {
  if (options?.path?.projectId !== fixtureProjectId) return notFoundResponse('读取评测结果列表', options?.path?.projectId)
  return fixtureResponse({ items: EVALUATION_SCENES.has(activeScene.value.id) ? evaluationResultFixtures() : [], nextCursor: null })
}

const runningEvaluationRunId = 'evaluation-run-pending'

function evaluationRunNotFound(runId: string): Promise<FixtureError> {
  return Promise.resolve({
    data: undefined,
    error: {
      type: 'about:blank', title: '评测不存在或尚未结束', status: 404, detail: 'Evaluation run has no terminal result.',
      instance: `/fixture-preview/evaluation-runs/${runId}`, requestId: 'fixture-evaluation-run-not-found', diagnosticCode: 'LW_EVALUATION_RUN_NOT_FOUND', retryable: false,
    } satisfies ProblemDetails,
  })
}

/** A retried step waiting for resource approval; the first attempt failed on the platform side. */
function runningEvaluationProgress(): EvaluationRunTaskProgressSchema {
  return {
    runId: runningEvaluationRunId, projectId: fixtureProjectId, revision: 5, state: 'running', page: 1, pageSize: 20, hasMore: false,
    current: [
      { stepRunId: 'evaluation-step-gate', progress: { attempt: 1, executionGeneration: 1, taskRunId: 'task-run-gate-1', phase: 'succeeded', diagnosticCode: 'LW_OJ_ACCEPTED', nextSteps: [] } },
      {
        stepRunId: 'evaluation-step-score',
        progress: { attempt: 2, taskRunId: 'task-run-score-2', phase: 'waiting_resource_approval', resourceRequestId: 'resource-request-eval-2', resourceState: 'reviewing', nextSteps: ['getOwnProjectEvaluationTaskProgress'] },
      },
    ],
    history: [
      { stepRunId: 'evaluation-step-score', progress: { attempt: 1, executionGeneration: 1, taskRunId: 'task-run-score-1', phase: 'failed', diagnosticCode: 'LW_OJ_JOB_FAILED', nextSteps: [] } },
    ],
  }
}

export async function getOwnProjectEvaluationTaskProgress(options?: FixtureOptions): Promise<FixtureResult<EvaluationRunTaskProgressSchema>> {
  const runId = options?.path?.runId
  if (!EVALUATION_SCENES.has(activeScene.value.id) || options?.path?.projectId !== fixtureProjectId || runId !== runningEvaluationRunId) {
    return notFoundResponse('读取评测进度', runId)
  }
  return fixtureResponse(runningEvaluationProgress())
}
export async function getOwnProjectEvaluationTaskForSubmission(options?: FixtureOptions): Promise<FixtureResult<EvaluationRunTaskProgressSchema>> {
  const submissionId = options?.path?.submissionId
  if (!EVALUATION_SCENES.has(activeScene.value.id) || options?.path?.projectId !== fixtureProjectId || submissionId !== 'submission-pending') {
    return notFoundResponse('读取提交的评测进度', submissionId)
  }
  return fixtureResponse(runningEvaluationProgress())
}
export async function cancelProjectAgentRun(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('取消项目任务') }
export async function createProjectAgentRun(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('创建项目任务') }
export async function createProjectWorkConfigurationRun(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('创建 Work 配置任务') }
export async function retryProjectAgentRunTrack(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('重试项目任务') }
export async function completeProjectProblemPackageUpload(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('完成材料上传') }
export async function createProjectProblemPackageUpload(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('创建材料上传') }
export async function listProjectResourceUsage(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('读取资源用量') }
export async function createProjectLlmPolicy(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('保存项目 AI 设置') }
export async function getProjectLlmPolicyOptions(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('读取项目 AI 选项') }
export async function approveProjectWorkConfigurationRun(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('批准 Work 配置计划') }
export async function getProjectWorkConfigurationPlan(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('读取 Work 配置计划') }
export async function appendProjectEnvironmentCandidateDecision(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('记录环境候选决定') }
export async function createEnvironmentTemplateRelease(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('发布环境模板') }
export async function getEnvironmentTemplateRelease(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('读取环境模板版本') }

/**
 * #209/#210 Credits preview: one granted account with a usage whose
 * measurement is still unknown after its hold was released, so the gallery
 * shows the "待核实，预占已释放" state instead of a zero. Writes and estimates
 * stay unsupported.
 */
export async function getProjectCredits(options?: FixtureOptions): Promise<FixtureResult<CreditAccountSchema>> {
  const projectId = options?.path?.projectId
  if (projectId !== fixtureProjectId) return notFoundResponse('读取项目 Credits', projectId)
  return fixtureResponse({
    projectId,
    currency: 'CREDITS',
    balance: '20.000000',
    available: '20.000000',
    reserved: '0.000000',
    spent: '0.000000',
    pending: '0.000000',
    unknown: '0.000000',
    platformAbsorbed: '0.000000',
    unknownCount: 1,
    revision: 3,
  })
}

export async function listProjectCreditHolds(options?: FixtureOptions): Promise<FixtureResult<CreditHoldPageSchema>> {
  const projectId = options?.path?.projectId
  if (projectId !== fixtureProjectId) return notFoundResponse('读取 Credits 预占', projectId)
  return fixtureResponse({
    items: [{
      id: '01900000-0000-7000-8000-00000000c101',
      projectId,
      requestId: '01900000-0000-7000-8000-00000000c201',
      businessKey: 'metered-usage',
      ownerClientId: 'environment-service',
      amount: '4.000000',
      remaining: '0.000000',
      state: 'released',
      measurementUnknown: true,
      createdAt: '2026-10-12T08:00:00.000Z',
      unknownSince: '2026-10-12T09:00:00.000Z',
      reconciliationDueAt: '2026-10-12T10:00:00.000Z',
      expiredUnknownAt: '2026-10-12T10:00:00.000Z',
    }],
    nextCursor: null,
  })
}

export async function listProjectCreditLedger(options?: FixtureOptions): Promise<FixtureResult<CreditLedgerPageSchema>> {
  const projectId = options?.path?.projectId
  if (projectId !== fixtureProjectId) return notFoundResponse('读取 Credits 账目', projectId)
  return fixtureResponse({
    items: [{
      id: '01900000-0000-7000-8000-00000000c001',
      projectId,
      kind: 'grant',
      operationId: '01900000-0000-7000-8000-00000000c301',
      reason: '课程合成额度',
      balanceDelta: '20.000000',
      reservedDelta: '0.000000',
      spentDelta: '0.000000',
      platformAbsorbed: '0.000000',
      rateSnapshots: [],
      createdAt: '2026-10-12T07:00:00.000Z',
    }],
    nextCursor: null,
  })
}

export async function createCreditAdminOperation(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('Credits 管理员操作') }
export async function estimateProjectResourceCost(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('资源费用估价') }
export async function estimateProjectBuildCost(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('镜像构建费用估价') }

// Notification center (#224) preview: one unread approval and one read reminder
// whose email delivery could not be confirmed. Marking read is a real write, so the preview refuses it.
function notificationsForPreview(): Notification[] {
  return [
    {
      id: '01900000-0000-7000-8000-00000000e001',
      eventId: '01900000-0000-7000-8000-00000000e101',
      projectId: fixtureProjectId,
      kind: 'resource_decision',
      objectUrl: `/researcher/resources?projectId=${fixtureProjectId}&requestId=request-authorized`,
      observedAt: now,
      summary: '资源申请已批准；请打开对象查看当前分配进度和实际可用期限。',
      emailState: 'disabled',
      emailAttempts: 0,
    },
    {
      id: '01900000-0000-7000-8000-00000000e002',
      eventId: '01900000-0000-7000-8000-00000000e102',
      projectId: fixtureProjectId,
      kind: 'lease_expiry_reminder',
      objectUrl: `/researcher/resources?projectId=${fixtureProjectId}&requestId=request-authorized`,
      observedAt: now,
      deadline: later,
      readAt: now,
      summary: '资源使用授权将在 1 小时后到期；到期后环境会停止，请及时保存数据。',
      emailState: 'delivery_unknown',
      emailAttempts: 1,
      emailDetail: '邮件可能已送达，系统无法确认；为避免重复发送，不会自动重发。',
      emailDiagnostic: 'LW_NOTIFICATION_SMTP_DELIVERY_UNKNOWN',
    },
  ]
}

export async function listNotifications(_options?: unknown): Promise<FixtureResult<NotificationPageSchema>> {
  return fixtureResponse({ items: notificationsForPreview(), nextCursor: null })
}
export async function getNotificationUnreadCount(_options?: unknown): Promise<FixtureResult<NotificationUnreadCountSchema>> {
  return fixtureResponse({ unreadCount: notificationsForPreview().filter((item) => !item.readAt).length })
}
export async function markNotificationRead(_options?: unknown): Promise<FixtureError> { return unsupportedResponse('标记通知已读') }
