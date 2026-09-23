import { activeScene, fixtureProjectId } from './scenes'
import type {
  AccessGrantSchema,
  ConsoleCapabilityAvailabilitySchema,
  EnvironmentAccessGrantPageSchema,
  EnvironmentEndpointSchema,
  EnvironmentInstanceSchema,
  EnvironmentOperation,
  EnvironmentOperationPageSchema,
  EnvironmentOperationSnapshot,
  GpuCatalogEntrySchema,
  ListEnvironmentsData,
  EnvironmentSummary,
  EnvironmentSummaryPageSchema,
  EnvironmentTemplateReleaseViewSchema,
  ProjectMembershipSchema,
  ProjectSchema,
  PlatformImageCatalogViewSchema,
  ProblemDetails,
  ResourceLeaseSchema,
  ResourceLeaseState,
  ResourceRequestSchema,
  ResourceRequestState,
  SshPublicKeySchema,
  WorkloadResources,
} from '../src/generated/contracts/types.gen'

type FixtureSuccess<T> = { data: T; error: undefined }
type FixtureError = { data: undefined; error: unknown }
type FixtureResult<T> = FixtureSuccess<T> | FixtureError

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

function currentEnvironmentState(): EnvironmentInstanceSchema['observedState'] | undefined {
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

const environmentLabels: Partial<Record<EnvironmentInstanceSchema['observedState'], string>> = {
  ready: '量子材料 · 交互实验环境',
  stopped: '量子材料 · 数据分析环境',
  deleted: '量子材料 · 历史实验环境',
  failed: '量子材料 · 置备诊断环境',
}

function environmentDisplayLabel(state: EnvironmentInstanceSchema['observedState']): string {
  return environmentLabels[state] ?? '量子材料 · 实验环境'
}

const sceneEnvironmentIds: Partial<Record<string, string>> = {
  'resource-authorized': 'env-physics-ready',
  'resource-reclaim': 'env-physics-ready',
  'workspace-members': 'env-workbench',
}

function environmentClassFor(id: string): EnvironmentInstanceSchema['class'] {
  return id === 'env-workbench' ? 'work' : 'experiment'
}

function environmentInstance(id: string): EnvironmentInstanceSchema {
  const state = currentEnvironmentState() ?? 'ready'
  const environmentClass = environmentClassFor(id)
  const deleted = state === 'deleted'
  const failed = state === 'failed'
  const stopped = state === 'stopped'
  const operation: EnvironmentOperation = {
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
  return {
    id,
    displayLabel: environmentDisplayLabel(state),
    projectId: fixtureProjectId,
    courseId: 'course-physics-2026',
    class: environmentClass,
    runtimeKind: 'container',
    releaseId: release.id,
    releaseVersion: release.version,
    ownerId: 'fixture-student',
    providerBinding: 'fixture-kubernetes',
    capacityBinding: 'fixture-capacity-claim',
    leaseId: 'lease-env-physics',
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
    releaseId: release.id,
    releaseVersion: release.version,
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

function releasesForScene(): EnvironmentTemplateReleaseViewSchema[] {
  return activeScene.value.id === 'teacher-template-empty' || activeScene.value.id === 'resource-empty'
    ? []
    : [release, vmRelease]
}

function projectRequests(): ResourceRequestSchema[] {
  switch (activeScene.value.id) {
    case 'resource-processing':
      return [makeRequest('request-reviewing', 'work-20260914-review', 'reviewing'), makeRequest('request-allocating', 'work-20260914-allocating', 'allocating')]
    case 'resource-authorized':
    case 'resource-reclaim':
      return [makeRequest('request-authorized', 'work-20260914-authorized', 'active')]
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
  query?: Partial<ListEnvironmentsData['query']>
}

export async function listProjects(): Promise<FixtureResult<ProjectSchema[]>> {
  return fixtureResponse(projectsForScene())
}

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

export async function listProjectMemberships(options?: FixtureOptions): Promise<FixtureResult<ProjectMembershipSchema[]>> {
  const projectId = options?.path?.projectId
  const selectedProject = knownProject(projectId)
  if (!selectedProject) return notFoundResponse('读取项目成员', projectId)
  if (selectedProject.state === 'archived') return fixtureResponse([])
  return fixtureResponse(activeScene.value.id === 'workspace-members' ? [
    { actorId: '周同学', projectId: fixtureProjectId, role: 'student', state: 'active', revision: 2, courseId: 'course-physics-2026', expiresAt: null },
    { actorId: '助教·陈老师', projectId: fixtureProjectId, role: 'teacher', state: 'active', revision: 1, courseId: 'course-physics-2026', expiresAt: null },
  ] : [])
}

export async function listEnvironments(options?: FixtureOptions): Promise<FixtureResult<EnvironmentSummaryPageSchema>> {
  const projectId = options?.query?.projectId
  const selectedProject = knownProject(projectId)
  if (!selectedProject) return notFoundResponse('读取环境（项目）', projectId)
  if (selectedProject.state === 'archived') {
    return fixtureResponse({ items: [], nextCursor: null, snapshotAt: now, snapshotSequence: 'fixture-42' })
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

export async function getEnvironment(options?: FixtureOptions): Promise<FixtureResult<EnvironmentInstanceSchema>> {
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
export async function listProjectResourceRequests(options?: FixtureOptions): Promise<FixtureResult<ResourceRequestSchema[]>> {
  const projectId = options?.path?.projectId
  if (projectId && projectId !== fixtureProjectId) return notFoundResponse('读取项目资源申请', projectId)
  return fixtureResponse(projectRequests())
}
export async function listProjectResourceLeases(options?: FixtureOptions): Promise<FixtureResult<ResourceLeaseSchema[]>> {
  const projectId = options?.path?.projectId
  if (projectId && projectId !== fixtureProjectId) return notFoundResponse('读取项目资源授权', projectId)
  return fixtureResponse(projectLeases())
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

export async function listResourceRequests(): Promise<FixtureResult<ResourceRequestSchema[]>> { return fixtureResponse(approvalRequests()) }
export async function listResourceLeases(): Promise<FixtureResult<ResourceLeaseSchema[]>> { return fixtureResponse(approvalLeases()) }
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
export async function getFrozenSubmission(): Promise<FixtureError> { return unsupportedResponse('读取冻结提交') }

export async function listPlatformImages(): Promise<FixtureResult<PlatformImageCatalogViewSchema>> {
  return fixtureResponse(platformImageCatalog)
}
export async function registerPlatformImage() { return unsupportedResponse('注册平台镜像') }
export async function repinPlatformImage() { return unsupportedResponse('重新固定平台镜像') }
export async function disablePlatformImage() { return unsupportedResponse('停用平台镜像') }
export async function createPlatformImageUpload() { return unsupportedResponse('创建平台镜像上传会话') }
export async function completePlatformImageUpload() { return unsupportedResponse('完成平台镜像导入') }

export async function listResourceGpuCatalog(): Promise<FixtureResult<GpuCatalogEntrySchema[]>> { return fixtureResponse(gpuCatalogFixture) }
export async function createResourceGpuCatalogEntry() { return unsupportedResponse('创建 GPU 目录项') }
