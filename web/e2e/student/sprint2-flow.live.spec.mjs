import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect, test } from '@playwright/test'
import {
  AUTH_STATE,
  createProjectByUi,
  configureProjectPolicyByUi,
  expectJson,
  navigateFromHomeByUi,
  pollEnvironmentCandidate,
  pollJson,
  selectProjectByUi,
  uuidv7,
} from '../support/live.mjs'
import {
  assertRealWorkGpuContainerCandidate,
  assertRealWorkVmCandidate,
  configureRealWorkBudgetByUi,
  cleanupWorkResources,
  createRealWorkPackage,
  ensureRealWorkRates,
  inspectRealWorkFinanceByUi,
  realWorkConfig,
  realWorkGpuConfig,
  realWorkResumeConfig,
  realWorkVmConfig,
  readResumablePublishedWork,
  selectRealWorkFinanceAdjustmentCharge,
  selectPendingWorkTaskResourceRequest,
  waitForSettledWorkUsageCharges,
  verifyRealWorkFinanceAdjustmentByUi,
  waitForDeletedEnvironment,
} from '../support/real-work.mjs'
import { readActorId } from '../support/real-experiment.mjs'
import { runTerminalCudaProbe } from '../support/real-gpu.mjs'
import { approveResourceRequestByUi } from '../support/real-resource.mjs'
import {
  createRealWorkSshIdentity,
  openPinnedSshSession,
  readRealWorkVmLicenseStatus,
  readRealWorkVmWorkspaceFile,
  runRealWorkVmCudaProbe,
  runPinnedSsh,
} from '../support/real-work-ssh.mjs'
import {
  addSshPublicKeyByUi as addStudentSshKeyByUi,
  deleteSshPublicKeyByUi as deleteStudentSshKeyByUi,
  issueEnvironmentSshAccessGrantByUi as issueWorkAccessGrantByUi,
  issueEnvironmentAccessGrantByUi,
  revokeEnvironmentAccessGrantByUi,
  waitForActiveAccessGrant,
} from '../support/ssh-access.mjs'
import { assertNoStuckProgress, auditAccessibility, installUsabilityGuards } from '../support/usability.mjs'

const PACKAGE_CONTENT = '# LabWeaver live Work fixture\n\nUse the managed environment.\n'
const GPU_MODE_LABELS = Object.freeze({
  exclusive: '独占',
  container_time_slice: '容器时间片',
  vm_vgpu: 'VM vGPU',
})
const GIB = 1024 ** 3
const VM_PERSISTENCE_MARKER_PATH = 'workspace/persistence-marker.txt'

// The agent worker runs one reserved dispatch at a time, so a journey can sit
// behind earlier runs before its own authoring starts. These ceilings cover a
// queued run plus the deployment's own fifteen minute per-candidate LLM bound
// and the image build that follows it.
const FULL_CHAIN_TIMEOUT_MS = 14_400_000
const AUTHORING_RUN_TIMEOUT_MS = 9_000_000
const CANDIDATE_BUILD_TIMEOUT_MS = 3_600_000
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
const EXISTING_PROJECT_ID = process.env.LABWEAVER_E2E_WORK_PROJECT_ID?.trim() ?? ''
const EXISTING_PROJECT_MODE = Boolean(EXISTING_PROJECT_ID)
const EXISTING_PROJECT_MODEL = 'qwen3.6:35b'
const REAL_WORK_VM = realWorkVmConfig()
const REAL_WORK_CONFIG = realWorkConfig({ virtualMachine: Boolean(REAL_WORK_VM) })
const REAL_WORK_RESUME = realWorkResumeConfig()
const REAL_WORK_GPU = realWorkGpuConfig()
const AUTHORING_RESOURCE_PROVIDER_BINDING =
  process.env.LABWEAVER_E2E_AUTHORING_PROVIDER_BINDING?.trim()
  || process.env.LABWEAVER_E2E_PROVIDER_BINDING?.trim()
  || 'container-primary-v1'
const WORK_PROVIDER_BINDING = REAL_WORK_VM?.providerBinding
  ?? process.env.LABWEAVER_E2E_PROVIDER_BINDING
  ?? 'kubernetes-work-local-hostpath'
const REAL_WORK_MODE = Boolean(REAL_WORK_CONFIG || REAL_WORK_RESUME || REAL_WORK_VM || EXISTING_PROJECT_MODE)
if (EXISTING_PROJECT_MODE && !UUID.test(EXISTING_PROJECT_ID)) {
  throw new Error('LABWEAVER_E2E_WORK_PROJECT_ID_INVALID')
}
if (EXISTING_PROJECT_MODE && REAL_WORK_RESUME) {
  throw new Error('LABWEAVER_E2E_WORK_PROJECT_AND_RESUME_CONFLICT')
}
if (EXISTING_PROJECT_MODE && !REAL_WORK_CONFIG && !REAL_WORK_VM) {
  throw new Error('LABWEAVER_E2E_WORK_PROJECT_REQUIRES_REAL_PROVIDER')
}
if (REAL_WORK_GPU && !REAL_WORK_MODE) throw new Error('LABWEAVER_E2E_WORK_GPU_REQUIRES_REAL_PROVIDER')
if (REAL_WORK_VM && REAL_WORK_GPU?.mode !== 'vm_vgpu') throw new Error('LABWEAVER_E2E_VM_REQUIRES_VM_VGPU')
if (REAL_WORK_GPU?.mode === 'vm_vgpu' && !REAL_WORK_VM) throw new Error('LABWEAVER_E2E_VM_VGPU_REQUIRES_VM_CONFIGURATION')

test.describe.configure({ timeout: FULL_CHAIN_TIMEOUT_MS })

function diagnosticCode(value) {
  return value?.diagnosticCode ?? value?.diagnostic_code ?? 'diagnostic missing'
}

function terminalRunState(value) {
  return ['succeeded', 'partially_succeeded', 'failed', 'cancelled'].includes(value)
}

const WORK_RESOURCE_REQUEST_TERMINAL_STATES = new Set(['expired', 'rejected', 'cancelled'])
const WORK_RESOURCE_REQUEST_ACTIVE_STATES = new Set(['reviewing', 'allocating', 'active', 'expiring'])
const WORK_RESOURCE_LEASE_TERMINAL_STATES = new Set(['expired', 'revoked'])
const WORK_RESOURCE_LEASE_ACTIVE_STATES = new Set(['allocating', 'active', 'expiring'])

async function readExistingProjectPolicy(request, projectId) {
  const policy = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/llm-egress-policies/active`),
    'REAL_WORK_EXISTING_PROJECT_POLICY_READ_FAILED',
  )
  if (
    policy?.projectId !== projectId
    || policy.binding?.model !== EXISTING_PROJECT_MODEL
    || !policy.budget
    || typeof policy.budget !== 'object'
    || Array.isArray(policy.budget)
  ) {
    throw new Error(`REAL_WORK_EXISTING_PROJECT_POLICY_INVALID:${policy?.binding?.model ?? 'missing'}`)
  }
  return policy
}

async function readExistingProjectResourceBudget(request, projectId, diagnostic = 'REAL_WORK_EXISTING_PROJECT_RESOURCE_BUDGET_READ_FAILED') {
  const response = await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/resource-budget`)
  if (response.status() === 404) {
    const bodyText = await response.text()
    let body
    try {
      body = JSON.parse(bodyText)
    } catch (error) {
      throw new Error(`${diagnostic}:404:invalid JSON`, { cause: error })
    }
    if (diagnosticCode(body) === 'LW_RESOURCE_BUDGET_NOT_FOUND') return null
    throw new Error(`${diagnostic}:404:${diagnosticCode(body)}`)
  }
  const budget = await expectJson(response, diagnostic)
  if (
    budget?.projectId !== projectId
    || !budget.limit
    || !budget.warningAt
  ) {
    throw new Error(`${diagnostic}:invalid`)
  }
  return budget
}

async function readWorkEnvironmentSummaries(request, projectId) {
  const items = []
  const seenCursors = new Set()
  let cursor = null
  for (;;) {
    const query = new URLSearchParams({ projectId, class: 'work', limit: '100' })
    if (cursor) query.set('cursor', cursor)
    const page = await expectJson(
      await request.get(`/api/v1/environments?${query.toString()}`),
      'REAL_WORK_EXISTING_PROJECT_ENVIRONMENTS_READ_FAILED',
    )
    if (!page || !Array.isArray(page.items)) {
      throw new Error('REAL_WORK_EXISTING_PROJECT_ENVIRONMENTS_INVALID')
    }
    for (const environment of page.items) {
      if (environment.projectId !== projectId || environment.class !== 'work') {
        throw new Error('REAL_WORK_EXISTING_PROJECT_ENVIRONMENT_SCOPE_INVALID')
      }
      items.push(environment)
    }
    const nextCursor = page.nextCursor ?? null
    if (!nextCursor) return items
    if (typeof nextCursor !== 'string' || seenCursors.has(nextCursor)) {
      throw new Error('REAL_WORK_EXISTING_PROJECT_ENVIRONMENTS_CURSOR_INVALID')
    }
    seenCursors.add(nextCursor)
    cursor = nextCursor
  }
}

async function assertExistingProjectWorkResourcesReleased(request, projectId) {
  const environments = await readWorkEnvironmentSummaries(request, projectId)
  for (const environment of environments) {
    if (environment.observedState !== 'deleted') {
      throw new Error(`REAL_WORK_EXISTING_PROJECT_ENVIRONMENT_ACTIVE:${environment.id}:${environment.observedState}`)
    }
  }

  const requests = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/resource-requests`),
    'REAL_WORK_EXISTING_PROJECT_RESOURCE_REQUESTS_READ_FAILED',
  )
  if (!Array.isArray(requests)) throw new Error('REAL_WORK_EXISTING_PROJECT_RESOURCE_REQUESTS_INVALID')

  for (const resourceRequest of requests) {
    if (resourceRequest?.projectId !== projectId) {
      throw new Error(`REAL_WORK_EXISTING_PROJECT_RESOURCE_REQUEST_SCOPE_INVALID:${resourceRequest?.id ?? 'missing'}`)
    }
    if (WORK_RESOURCE_REQUEST_TERMINAL_STATES.has(resourceRequest.state)) continue
    if (WORK_RESOURCE_REQUEST_ACTIVE_STATES.has(resourceRequest.state)) {
      throw new Error(`REAL_WORK_EXISTING_PROJECT_RESOURCE_REQUEST_ACTIVE:${resourceRequest.id}:${resourceRequest.state}`)
    }
    throw new Error(`REAL_WORK_EXISTING_PROJECT_RESOURCE_REQUEST_STATE_INVALID:${resourceRequest.id}:${resourceRequest.state}`)
  }

  const leases = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/resource-leases`),
    'REAL_WORK_EXISTING_PROJECT_RESOURCE_LEASES_READ_FAILED',
  )
  if (!Array.isArray(leases)) throw new Error('REAL_WORK_EXISTING_PROJECT_RESOURCE_LEASES_INVALID')
  for (const lease of leases) {
    if (WORK_RESOURCE_LEASE_TERMINAL_STATES.has(lease.state)) continue
    if (WORK_RESOURCE_LEASE_ACTIVE_STATES.has(lease.state)) {
      throw new Error(`REAL_WORK_EXISTING_PROJECT_RESOURCE_LEASE_ACTIVE:${lease.id}:${lease.state}`)
    }
    throw new Error(`REAL_WORK_EXISTING_PROJECT_RESOURCE_LEASE_STATE_INVALID:${lease.id}:${lease.state}`)
  }
}

async function approvePendingAgentTaskResourceByUi(adminPage, {
  projectId,
  run,
  runId,
  packageId,
  policyId,
  policyRevision,
  studentActorId,
  trackKind,
  purposeKind,
  environmentClass = null,
  environmentId = null,
  environmentRevision = null,
  approvedRequestIds = new Set(),
}) {
  if (
    run.id !== runId
    || run.projectId !== projectId
    || run.packageId !== packageId
    || run.policyId !== policyId
    || run.policyRevision !== policyRevision
    || run.purpose?.kind !== purposeKind
    || (purposeKind === 'authoring' && run.purpose.environmentClass !== environmentClass)
    || (purposeKind === 'work_configuration'
      && (run.purpose.environmentId !== environmentId
        || run.purpose.environmentRevision !== environmentRevision
        || run.purpose.actorId !== studentActorId))
  ) {
    throw new Error('WORK_TASK_RESOURCE_RUN_SCOPE_INVALID')
  }

  const track = run.tracks?.find((item) => item.kind === trackKind)
  if (!track) throw new Error('WORK_TASK_RESOURCE_TRACK_MISSING')
  const activeAttempts = track.attempts?.filter((attempt) => (
    ['pending', 'running', 'repairing', 'awaiting_approval'].includes(attempt.state)
  )) ?? []
  if (activeAttempts.length === 0) return
  if (activeAttempts.length !== 1 || activeAttempts[0].number !== 1) {
    throw new Error('WORK_TASK_RESOURCE_ATTEMPT_UNEXPECTED')
  }
  const activeAttempt = activeAttempts[0]

  const requests = await expectJson(
    await adminPage.request.get(`/api/v1/projects/${projectId}/resource-requests`),
    'WORK_TASK_RESOURCE_REQUESTS_READ_FAILED',
  )
  const request = selectPendingWorkTaskResourceRequest(requests, {
    projectId,
    runId,
    trackKind,
    attemptNumber: activeAttempt.number,
    studentActorId,
    ignoredRequestIds: approvedRequestIds,
  })
  if (request?.state === 'reviewing') {
    if (!Number.isInteger(request.requestedDurationSeconds) || request.requestedDurationSeconds <= 0) {
      throw new Error(`WORK_TASK_RESOURCE_DURATION_INVALID:${request.id}`)
    }
    await approveResourceRequestByUi(adminPage, {
      requestKey: request.requestKey,
      projectId,
      requestId: request.id,
      requesterId: studentActorId,
      durationSeconds: request.requestedDurationSeconds,
      providerBinding: AUTHORING_RESOURCE_PROVIDER_BINDING,
      onTaskOwnerRelease: async ({ requestId, leaseId }) => {
        // A task-owned lease is intentionally short-lived: the task worker
        // releases it before the same run advances to its next attempt. The
        // request selector above already fenced this approval to the current
        // run, track, attempt, requester, and taskRunId. Requiring the lease
        // to remain active here would deadlock schema-repair/resource chains;
        // the caller's run poll remains the authority for task success/failure.
        if (requestId !== request.id || leaseId === '') {
          throw new Error('WORK_TASK_RESOURCE_LEASE_SCOPE_INVALID')
        }
        return true
      },
    })
    approvedRequestIds.add(request.id)
  }
}

async function publishWorkTemplate(page, project, packageCopy = null, { adminPage, studentActorId }) {
  const packageDirectory = packageCopy?.directory ?? await mkdtemp(join(tmpdir(), 'labweaver-work-package-'))
  const ownsPackageDirectory = !packageCopy
  const approvedTaskResourceRequestIds = new Set()
  try {
    await page.goto(`/researcher/software?projectId=${encodeURIComponent(project.id)}`, {
      waitUntil: 'domcontentloaded',
    })
    await selectProjectByUi(page, project.id)
    await page.getByRole('button', { name: '生成 Work 模板', exact: true }).click()
    await expect(page.getByRole('heading', { name: '生成 Work 模板', exact: true })).toBeVisible()

    const fileInput = page.getByTestId('work-template-file-input')
    if (ownsPackageDirectory) await writeFile(join(packageDirectory, 'README.md'), PACKAGE_CONTENT, 'utf8')
    await fileInput.setInputFiles(packageDirectory)
    await expect(page.getByRole('list', { name: '待上传材料文件', exact: true })).toContainText('README.md')

    const packageResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && /\/api\/v1\/projects\/[^/]+\/problem-package-uploads\/[^/]+\/complete$/.test(url.pathname)
    })
    await page.getByRole('button', { name: '上传材料包', exact: true }).click()
    const packageResponse = await packageResponsePromise
    const packageData = await expectJson(packageResponse, 'WORK_PACKAGE_UPLOAD_FAILED')
    expect(packageData).toMatchObject({ projectId: project.id, revision: expect.any(Number) })
    await expect(page.locator('.package-summary').getByText(/材料包已归档：/)).toBeVisible({ timeout: 120_000 })

    const runResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${project.id}/agent-runs`
    })
    await page.getByRole('button', { name: '启动 Work 模板生成', exact: true }).click()
    const runResponse = await runResponsePromise
    const acceptedRun = await expectJson(runResponse, 'WORK_TEMPLATE_RUN_CREATE_FAILED')
    expect(acceptedRun).toMatchObject({
      id: expect.any(String),
      projectId: project.id,
      packageId: packageData.id,
      policyId: expect.any(String),
      policyRevision: expect.any(Number),
    })
    expect(acceptedRun.purpose?.environmentClass ?? acceptedRun.environmentClass).toBe('work')

    const run = await pollJson(
      page.request,
      `/api/v1/projects/${project.id}/agent-runs/${acceptedRun.id}`,
      async (value) => {
        if (!terminalRunState(value.state)) {
          await approvePendingAgentTaskResourceByUi(adminPage, {
            projectId: project.id,
            run: value,
            runId: acceptedRun.id,
            packageId: packageData.id,
            policyId: acceptedRun.policyId,
            policyRevision: acceptedRun.policyRevision,
            studentActorId,
            trackKind: 'environment',
            purposeKind: 'authoring',
            environmentClass: 'work',
            approvedRequestIds: approvedTaskResourceRequestIds,
          })
        }
        return terminalRunState(value.state)
      },
      'WORK_TEMPLATE_RUN_STATUS_FAILED',
      // A real authoring run drives the sandbox CLI against the deployment's
      // model, so it can take as long as the harness LLM timeout allows.
      AUTHORING_RUN_TIMEOUT_MS,
    )
    if (run.state !== 'succeeded') {
      throw new Error(`WORK_TEMPLATE_RUN_FAILED:${run.state}:${run.tracks?.map((track) => track.attempts?.map(diagnosticCode).join(',')).join(';') ?? 'no tracks'}`)
    }
    const environmentTrack = run.tracks.find((track) => track.kind === 'environment')
    if (!environmentTrack?.candidateId) throw new Error('WORK_TEMPLATE_CANDIDATE_MISSING')

    const candidate = await pollEnvironmentCandidate(
      page.request,
      project.id,
      environmentTrack.candidateId,
      (value) => {
        if (REAL_WORK_VM) return Boolean(value.candidate)
        return ['succeeded', 'failed', 'cancelled'].includes(value.build?.state)
      },
      REAL_WORK_VM ? 'WORK_TEMPLATE_VM_CANDIDATE_READ_FAILED' : 'WORK_TEMPLATE_CANDIDATE_BUILD_STATUS_FAILED',
      CANDIDATE_BUILD_TIMEOUT_MS,
    )
    if (candidate.candidate?.spec?.class !== 'work') throw new Error('WORK_TEMPLATE_CANDIDATE_CLASS_INVALID')
    if (!candidate.imageArtifact) {
      throw new Error(`WORK_TEMPLATE_CANDIDATE_ARTIFACT_NOT_READY:${candidate.build?.diagnosticCode ?? 'artifact missing'}`)
    }
    if (REAL_WORK_MODE) {
      const runtime = candidate.candidate?.spec?.runtime
      if (REAL_WORK_VM) {
        assertRealWorkVmCandidate(candidate, REAL_WORK_VM, REAL_WORK_GPU)
      } else {
        expect(runtime).toMatchObject({
          kind: 'container',
          provider_binding: WORK_PROVIDER_BINDING,
          service_port: 8080,
          build_context: {
            artifactId: expect.any(String),
            objectVersion: expect.any(String),
          },
        })
        if (candidate.build?.state !== 'succeeded') {
          throw new Error(`WORK_TEMPLATE_CANDIDATE_BUILD_FAILED:${candidate.build?.diagnosticCode ?? 'build not succeeded'}`)
        }
        realContainerArtifact(candidate)
        if (REAL_WORK_GPU) assertRealWorkGpuContainerCandidate(candidate, WORK_PROVIDER_BINDING, REAL_WORK_GPU)
      }
      if (REAL_WORK_GPU) {
        expect(candidate.candidate?.spec?.resources?.gpu).toEqual({
          class: REAL_WORK_GPU.class,
          count: REAL_WORK_GPU.count,
        })
      }
    } else if (candidate.build?.state !== 'succeeded') {
      throw new Error(`WORK_TEMPLATE_CANDIDATE_BUILD_FAILED:${candidate.build?.diagnosticCode ?? 'build not succeeded'}`)
    }

    const candidateCard = page.getByTestId('work-template-candidate')
    await expect(candidateCard).toBeVisible({ timeout: 120_000 })
    if (REAL_WORK_VM) {
      await expect(candidateCard).toContainText(REAL_WORK_VM.baseDisk.binding, { timeout: 120_000 })
    } else {
      await expect(candidateCard).toContainText('构建完成', { timeout: 120_000 })
    }
    await candidateCard.getByTestId('work-template-candidate-confirmation').check()
    await candidateCard.getByPlaceholder('说明为什么批准这个 Work 环境候选').fill(
      REAL_WORK_VM
        ? '已核对 Work 环境候选规格、虚拟机基础镜像和项目安全约束。'
        : '已核对 Work 环境候选规格、容器 artifact 和项目安全约束。',
    )
    const approveButton = candidateCard.getByRole('button', { name: '批准环境候选', exact: true })
    await expect(approveButton).toBeEnabled()
    const approvalResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${project.id}/environment-candidates/${environmentTrack.candidateId}/decisions`
    })
    await approveButton.click()
    const approvalResponse = await approvalResponsePromise
    const approval = await expectJson(approvalResponse, 'WORK_TEMPLATE_CANDIDATE_APPROVAL_FAILED')
    expect(approval).toMatchObject({ id: expect.any(String), candidateId: environmentTrack.candidateId, decision: 'approved' })
    await expect(candidateCard).toContainText(`候选已批准：${approval.id}`)

    const releaseResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${project.id}/environment-template-releases`
    })
    await page.getByTestId('work-template-release-button').click()
    const releaseAccepted = await expectJson(await releaseResponsePromise, 'WORK_TEMPLATE_RELEASE_CREATE_FAILED')
    expect(releaseAccepted).toMatchObject({ operationId: expect.any(String), statusUrl: expect.stringContaining('/environment-template-releases/') })
    const release = await pollJson(
      page.request,
      releaseAccepted.statusUrl,
      (value) => value.id && value.projectId === project.id && value.candidateId === environmentTrack.candidateId,
      'WORK_TEMPLATE_RELEASE_STATUS_FAILED',
      120_000,
    )
    expect(release).toMatchObject({
      projectId: project.id,
      runtimeKind: REAL_WORK_VM ? 'virtual_machine' : 'container',
      version: expect.any(Number),
    })
    if (REAL_WORK_VM) expect(release.artifact).toEqual(candidate.imageArtifact)
    await expect(page.getByTestId('work-template-resource-link')).toBeVisible()
    return { packageData, run, candidate, release }
  } finally {
    if (ownsPackageDirectory) await rm(packageDirectory, { recursive: true, force: true })
  }
}

async function waitForResourceRequest(request, projectId, requestId) {
  const snapshot = await pollJson(
    request,
    `/api/v1/projects/${projectId}/resource-requests`,
    (value) => Array.isArray(value) && value.some((item) => item.id === requestId && ['active', 'rejected', 'cancelled'].includes(item.state)),
    'RESOURCE_REQUEST_STATUS_FAILED',
    240_000,
  )
  const item = snapshot.find((candidate) => candidate.id === requestId)
  if (!item || item.state !== 'active') throw new Error(`RESOURCE_REQUEST_NOT_ACTIVE:${item?.state ?? 'missing'}:${item?.diagnosticCode ?? 'diagnostic missing'}`)
  return item
}

async function waitForLease(request, projectId, requestId) {
  const snapshot = await pollJson(
    request,
    `/api/v1/projects/${projectId}/resource-leases`,
    (value) => Array.isArray(value) && value.some((item) => item.requestId === requestId && ['active', 'revoked', 'expired'].includes(item.state)),
    'RESOURCE_LEASE_STATUS_FAILED',
    240_000,
  )
  const item = snapshot.find((candidate) => candidate.requestId === requestId)
  if (!item || item.state !== 'active') throw new Error(`RESOURCE_LEASE_NOT_ACTIVE:${item?.state ?? 'missing'}:${item?.revokeReasonCode ?? 'diagnostic missing'}`)
  return item
}

async function waitForEnvironment(request, environmentId, expectedState) {
  const value = await pollJson(
    request,
    `/api/v1/environments/${environmentId}`,
    (item) => item.observedState === expectedState || ['failed', 'deleted'].includes(item.observedState),
    `ENVIRONMENT_${expectedState.toUpperCase()}_STATUS_FAILED`,
    240_000,
  )
  if (value.observedState !== expectedState) throw new Error(`ENVIRONMENT_STATE_INVALID:${value.observedState}:${value.lastDiagnosticCode ?? 'diagnostic missing'}`)
  return value
}

async function expectProblem(response, expectedStatus, expectedDiagnostic, label) {
  const bodyText = await response.text()
  let body
  try {
    body = JSON.parse(bodyText)
  } catch (error) {
    throw new Error(`${label}:invalid JSON:${bodyText.slice(0, 2000)}`, { cause: error })
  }
  if (response.status() !== expectedStatus) {
    throw new Error(`${label}:status ${response.status()} expected ${expectedStatus}:${bodyText.slice(0, 2000)}`)
  }
  if (diagnosticCode(body) !== expectedDiagnostic) {
    throw new Error(`${label}:diagnostic ${diagnosticCode(body)} expected ${expectedDiagnostic}`)
  }
  return body
}

async function verifyCrossProjectEnvironmentDenied(browser, baseURL, projectId, environmentId) {
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.teacher })
  try {
    const paths = [
      `/api/v1/environments?projectId=${encodeURIComponent(projectId)}`,
      `/api/v1/environments/${environmentId}`,
      `/api/v1/environments/${environmentId}/endpoints`,
      `/api/v1/projects/${projectId}/resource-requests`,
      `/api/v1/projects/${projectId}/resource-leases`,
    ]
    for (const path of paths) {
      await expectProblem(
        await context.request.get(path),
        403,
        'LW_AUTH_SCOPE_DENIED',
        `CROSS_PROJECT_ENVIRONMENT_ACCESS_DENIED:${path}`,
      )
    }
  } finally {
    await context.close()
  }
}

async function assertConnectionBlockedAfterLeaseRevoke(page, projectId, environment) {
  await page.goto(`/student/environments?projectId=${encodeURIComponent(projectId)}&environmentId=${encodeURIComponent(environment.id)}`, {
    waitUntil: 'domcontentloaded',
  })
  await expect(page.getByRole('heading', { name: '项目环境控制台', exact: true })).toBeVisible({ timeout: 120_000 })
  await expect(page.getByText('环境已停止，启动后才能签发访问授权。', { exact: true })).toBeVisible({ timeout: 120_000 })
  const createButton = page.getByRole('button', { name: '签发访问授权', exact: true })
  if (await createButton.count() > 0) await expect(createButton).toBeDisabled()
}

function realContainerArtifact(candidate) {
  const artifact = candidate?.imageArtifact ?? candidate?.build?.artifact
  if (!artifact || artifact.kind !== 'container') throw new Error('REAL_WORK_CONTAINER_ARTIFACT_MISSING')
  if (typeof artifact.repository !== 'string' || artifact.repository.trim() === '') {
    throw new Error('REAL_WORK_CONTAINER_REPOSITORY_MISSING')
  }
  if (!/^sha256:[0-9a-f]{64}$/i.test(artifact.digest ?? '')) {
    throw new Error('REAL_WORK_CONTAINER_DIGEST_INVALID')
  }
  if (artifact.digest.toLowerCase() === REAL_WORK_CONFIG.goldenBaseDigest) {
    throw new Error('REAL_WORK_BUILD_REUSED_GOLDEN_BASE_DIGEST')
  }
  return artifact
}

async function issueWorkAccessGrant(page, projectId, environment) {
  const issued = await issueEnvironmentAccessGrantByUi(page, projectId, environment, 'http')
  if (!issued.endpointGrant.connectUrl) throw new Error('REAL_WORK_ACCESS_GRANT_HTTP_CONNECTION_MISSING')
  return { ...issued, httpGrant: issued.endpointGrant }
}

async function readWorkEndpoint(request, connectUrl, label) {
  const response = await request.get(connectUrl)
  const body = await response.text()
  if (!response.ok()) throw new Error(`${label}:${response.status()}:${body.slice(0, 2000)}`)
  expect(response.status()).toBe(200)
  return body
}

async function readWorkFile(request, connectUrl, fileName, label) {
  const base = connectUrl.endsWith('/') ? connectUrl : `${connectUrl}/`
  return await readWorkEndpoint(request, `${base}${fileName}`, label)
}

function httpEndpointGrant(grant) {
  const httpGrant = grant?.endpointGrants?.find(
    (endpointGrant) => (endpointGrant.protocol === 'http' || endpointGrant.protocol === 'https')
      && typeof endpointGrant.connectUrl === 'string',
  )
  if (!httpGrant?.connectUrl) throw new Error('REAL_WORK_HTTP_ENDPOINT_GRANT_MISSING')
  return httpGrant
}

test('student provisions a Work environment, configures it, and releases its capacity', async ({ page, browser, baseURL }) => {
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')
  if (REAL_WORK_RESUME && REAL_WORK_CONFIG) throw new Error('REAL_WORK_RESUME_AND_FULL_PROVIDER_CONFIG_CONFLICT')
  const guards = installUsabilityGuards(page)

  await expectProblem(
    await page.request.get('/api/v1/resource-requests'),
    403,
    'LW_AUTH_SCOPE_DENIED',
    'STUDENT_GLOBAL_RESOURCE_REQUESTS_DENIED',
  )
  await expectProblem(
    await page.request.get('/api/v1/resource-leases'),
    403,
    'LW_AUTH_SCOPE_DENIED',
    'STUDENT_GLOBAL_RESOURCE_LEASES_DENIED',
  )

  const resumed = REAL_WORK_RESUME
    ? await readResumablePublishedWork(page.request, REAL_WORK_RESUME, { gpu: REAL_WORK_GPU, vm: REAL_WORK_VM })
    : null
  const studentActorId = await readActorId(page.request)
  let existingProjectPolicy = null
  let project
  if (resumed) {
    project = resumed.project
    await page.goto(`/researcher/workspaces?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
  } else if (EXISTING_PROJECT_MODE) {
    await navigateFromHomeByUi(page, '项目与工作空间')
    project = await expectJson(
      await page.request.get(`/api/v1/projects/${encodeURIComponent(EXISTING_PROJECT_ID)}`),
      'REAL_WORK_EXISTING_PROJECT_READ_FAILED',
    )
    if (project.id !== EXISTING_PROJECT_ID || project.ownerActorId !== studentActorId) {
      throw new Error('REAL_WORK_EXISTING_PROJECT_OWNERSHIP_INVALID')
    }
    existingProjectPolicy = await readExistingProjectPolicy(page.request, project.id)
    await assertExistingProjectWorkResourcesReleased(page.request, project.id)
  } else {
    project = await createProjectByUi(page, `live-work-${Date.now()}-${uuidv7().slice(0, 8)}`)
  }
  await selectProjectByUi(page, project.id)
  if (!resumed && !EXISTING_PROJECT_MODE) await configureProjectPolicyByUi(page, project.id)
  const packageCopy = !resumed && (REAL_WORK_VM || REAL_WORK_CONFIG)
    ? await createRealWorkPackage(REAL_WORK_VM ? null : REAL_WORK_CONFIG.goldenBaseImage, {
      gpu: REAL_WORK_GPU,
      providerBinding: WORK_PROVIDER_BINDING,
      vm: REAL_WORK_VM,
    })
    : null
  let trackedEnvironmentId = null
  let trackedLeaseId = null
  let trackedRequestId = null
  let vmSshIdentity = null
  let vmSshKey = null
  let vmSshSession = null
  let adminContext = null
  let adminPage = null
  let workRates = null
  let gpuRate = null
  let baselineChargeIds = new Set()
  let existingResourceBudget = null
  let primaryFailure = null
  const cleanupFailures = []
  try {
    adminContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
    adminPage = await adminContext.newPage()
    if (EXISTING_PROJECT_MODE) {
      existingResourceBudget = await readExistingProjectResourceBudget(adminPage.request, project.id)
    }
    const { packageData, release } = resumed
      ? { packageData: resumed.packageData, release: resumed.release }
      : await publishWorkTemplate(page, project, packageCopy, { adminPage, studentActorId })
    if (resumed && REAL_WORK_GPU && !REAL_WORK_VM) {
      assertRealWorkGpuContainerCandidate(resumed.candidateView, WORK_PROVIDER_BINDING, REAL_WORK_GPU)
    }
    const expectedSeedMarker = packageCopy?.seedMarker ?? resumed?.seedMarker
    const expectedPersistenceMarker = packageCopy?.persistenceMarker ?? resumed?.persistenceMarker

    if (REAL_WORK_MODE) {
      workRates = await ensureRealWorkRates(browser, baseURL, { gpu: REAL_WORK_GPU })
      if (REAL_WORK_GPU) {
        const matchingGpuRates = workRates.filter((rate) => (
          rate.unit === 'gpu_unit_second'
          && rate.gpuClass === REAL_WORK_GPU.class
          && rate.gpuMode === REAL_WORK_GPU.mode
        ))
        if (matchingGpuRates.length !== 1) {
          throw new Error(`REAL_WORK_GPU_RATE_READBACK_AMBIGUOUS:${REAL_WORK_GPU.class}:${REAL_WORK_GPU.mode}`)
        }
        [gpuRate] = matchingGpuRates
      }
      const baselineCharges = await expectJson(
        await adminPage.request.get(`/api/v1/projects/${encodeURIComponent(project.id)}/charges`),
        'REAL_WORK_BASELINE_CHARGES_READ_FAILED',
      )
      if (!Array.isArray(baselineCharges)) throw new Error('REAL_WORK_BASELINE_CHARGES_INVALID')
      baselineChargeIds = new Set(baselineCharges.map((charge) => charge.id).filter((id) => typeof id === 'string' && id !== ''))
      if (!EXISTING_PROJECT_MODE) {
        await configureRealWorkBudgetByUi(browser, baseURL, project.id)
      }
    }

    await page.goto(`/researcher/resources?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('heading', { name: '资源申请', exact: true, level: 2 })).toBeVisible()
    await selectProjectByUi(page, project.id)
    const releaseSelect = page.getByLabel('已发布版本')
    await expect(releaseSelect.locator(`option[value="${release.id}:${release.version}"]`)).toHaveCount(1, { timeout: 120_000 })
    await releaseSelect.selectOption(`${release.id}:${release.version}`)
    if (REAL_WORK_GPU) {
      const requiredRuntimeKind = REAL_WORK_VM ? 'virtual_machine' : 'container'
      if (release.runtimeKind !== requiredRuntimeKind) {
        throw new Error(`REAL_WORK_GPU_RUNTIME_MISMATCH:${REAL_WORK_GPU.mode}:${release.runtimeKind}`)
      }
      const gpuSelect = page.getByRole('combobox', { name: 'GPU 目录项（可选）', exact: true })
      await expect(gpuSelect).toBeEnabled({ timeout: 120_000 })
      const gpuValue = await gpuSelect.locator('option').evaluateAll((options, target) => {
        const expected = `${target.class} · ${target.modeLabel}`
        return options.find((option) => (option.textContent ?? '').trim().startsWith(expected))?.value ?? null
      }, { class: REAL_WORK_GPU.class, modeLabel: GPU_MODE_LABELS[REAL_WORK_GPU.mode] })
      if (!gpuValue) throw new Error(`REAL_WORK_GPU_CATALOG_OPTION_MISSING:${REAL_WORK_GPU.class}:${REAL_WORK_GPU.mode}`)
      await gpuSelect.selectOption(gpuValue)
      await expect(page.locator('.gpu-detail')).toContainText(`${REAL_WORK_GPU.class} · ${GPU_MODE_LABELS[REAL_WORK_GPU.mode]}`)
      const gpuCount = page.getByLabel('GPU 数量', { exact: true })
      if (await gpuCount.isEditable()) await gpuCount.fill(String(REAL_WORK_GPU.count))
      else await expect(gpuCount).toHaveValue(String(REAL_WORK_GPU.count))
    }
    await page.getByLabel('CPU（m）').fill('1000')
    await page.getByLabel('时长（小时）').fill('1')
    await page.getByLabel('内存（GiB）').fill('2')
    const vmBaseDiskCapacityBytes = REAL_WORK_VM ? release.artifact?.base_disk?.capacityBytes : null
    if (REAL_WORK_VM && (!Number.isSafeInteger(vmBaseDiskCapacityBytes) || vmBaseDiskCapacityBytes < 1)) {
      throw new Error('REAL_WORK_VM_RELEASE_BASE_DISK_CAPACITY_INVALID')
    }
    const storageGiB = REAL_WORK_VM ? Math.max(16, Math.ceil(vmBaseDiskCapacityBytes / GIB)) : 10
    await page.getByLabel('存储（GiB）').fill(String(storageGiB))
    const resourceResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST' && url.pathname === '/api/v1/resource-requests'
    })
    await page.getByRole('button', { name: '提交资源申请', exact: true }).click()
    const resourceResponse = await resourceResponsePromise
    const accepted = await expectJson(resourceResponse, 'RESOURCE_REQUEST_CREATE_FAILED')
    const onAcceptedResourceRequest = (acceptedRequest, response) => {
      if (typeof acceptedRequest?.requestId !== 'string' || acceptedRequest.requestId === '') {
        throw new Error('RESOURCE_REQUEST_CREATE_ID_MISSING')
      }
      trackedRequestId = acceptedRequest.requestId
      const body = response.request().postDataJSON()
      const environmentId = body?.target?.environmentId ?? acceptedRequest.environmentId
      if (typeof environmentId === 'string' && environmentId !== '') trackedEnvironmentId = environmentId
      return body
    }
    const requestBody = onAcceptedResourceRequest(accepted, resourceResponse)
    requestBody.requestId = accepted.requestId
    expect(accepted).toMatchObject({ requestId: expect.any(String) })
    expect(requestBody).toMatchObject({ projectId: project.id, target: { kind: 'environment', releaseId: release.id, releaseVersion: release.version } })
    if (REAL_WORK_GPU) {
      expect(requestBody.resources?.gpu).toEqual({ class: REAL_WORK_GPU.class, count: REAL_WORK_GPU.count })
    }
    const environmentId = requestBody.target.environmentId
    trackedEnvironmentId = environmentId

    const approval = await approveResourceRequestByUi(adminPage, {
      requestKey: requestBody.requestKey,
      projectId: project.id,
      requestId: requestBody.requestId,
      environmentId,
      requesterId: studentActorId,
      durationSeconds: requestBody.durationSeconds,
      providerBinding: WORK_PROVIDER_BINDING,
    })
    trackedLeaseId = approval.leaseId
    const activeRequest = await waitForResourceRequest(page.request, project.id, accepted.requestId)
    expect(activeRequest).toMatchObject({ id: accepted.requestId, projectId: project.id, state: 'active' })
    const lease = await waitForLease(page.request, project.id, accepted.requestId)
    trackedLeaseId = lease.id
    expect(lease).toMatchObject({
      id: approval.leaseId,
      requestId: activeRequest.id,
      claimId: expect.any(String),
      state: 'active',
      revision: expect.any(Number),
    })

    await page.reload({ waitUntil: 'domcontentloaded' })
    const connectLink = page.getByRole('link', { name: '连接', exact: true })
    await expect(connectLink).toBeVisible({ timeout: 120_000 })
    await connectLink.click()
    await expect(page).toHaveURL(new RegExp(`[?&]environmentId=${encodeURIComponent(environmentId)}(?:&|$)`))
    // The console titles a Work environment `work-<environment id>`, so match the
    // rendered heading by containment rather than by an exact id comparison.
    await expect(
      page.locator('.resource-title-row').getByRole('heading', { name: environmentId }),
    ).toBeVisible({ timeout: 120_000 })
    await expect(page.locator('.env-meta-grid')).toContainText(REAL_WORK_VM ? '虚拟机' : '容器')
    const environment = await waitForEnvironment(page.request, environmentId, 'ready')
    expect(environment.class).toBe('work')
    expect(environment.projectId).toBe(project.id)
    expect(environment.providerBinding).toBe(WORK_PROVIDER_BINDING)
    expect(environment.revision).toEqual(expect.any(Number))
    const endpointsResponse = await page.request.get(`/api/v1/environments/${environmentId}/endpoints`)
    const endpoints = await expectJson(endpointsResponse, 'WORK_ENVIRONMENT_ENDPOINTS_READ_FAILED')
    expect(Array.isArray(endpoints.items)).toBe(true)
    if (endpoints.items.length === 0) throw new Error('WORK_ENVIRONMENT_ENDPOINTS_MISSING')
    const endpointIds = endpoints.items.map((endpoint) => endpoint.id)
    expect(new Set(endpointIds).size).toBe(endpointIds.length)
    await verifyCrossProjectEnvironmentDenied(browser, baseURL, project.id, environmentId)

    let accessGrant
    let expectedAccessGrantId
    let sshEndpointGrant = null
    if (REAL_WORK_VM) {
      vmSshIdentity = await createRealWorkSshIdentity()
      await addStudentSshKeyByUi(page, vmSshIdentity, (acceptedKey) => {
        vmSshKey = acceptedKey
      })
      const issued = await issueWorkAccessGrantByUi(page, project.id, environment)
      accessGrant = issued.grant
      expectedAccessGrantId = accessGrant.id
      sshEndpointGrant = issued.endpointGrant
    } else {
      await expect(page.getByRole('button', { name: '签发访问授权', exact: true })).toBeVisible({ timeout: 120_000 })
      const accessGrantResponsePromise = page.waitForResponse((response) => {
        const url = new URL(response.url())
        return response.request().method() === 'POST'
          && url.pathname === `/api/v1/environments/${environmentId}/access-grants`
      })
      await page.getByRole('button', { name: '签发访问授权', exact: true }).click()
      const accessGrantResponse = await accessGrantResponsePromise
      const requestedAccessGrant = await expectJson(accessGrantResponse, 'WORK_ACCESS_GRANT_CREATE_FAILED')
      expect(requestedAccessGrant).toMatchObject({
        id: expect.any(String),
        projectId: project.id,
        environmentId,
        environmentRevision: environment.revision,
        state: 'requested',
      })
      expectedAccessGrantId = requestedAccessGrant.id
      accessGrant = await waitForActiveAccessGrant(page.request, requestedAccessGrant.id)
    }
    let revocationTargetGrant = accessGrant
    expect(accessGrant).toMatchObject({
      id: expectedAccessGrantId,
      projectId: project.id,
      environmentId,
      environmentRevision: environment.revision,
      state: 'active',
      revision: expect.any(Number),
    })
    expect(accessGrant.endpointGrants).toHaveLength(endpoints.items.length)
    const endpointsById = new Map(endpoints.items.map((endpoint) => [endpoint.id, endpoint]))
    for (const endpointGrant of accessGrant.endpointGrants) {
      const endpoint = endpointsById.get(endpointGrant.endpointId)
      if (!endpoint) throw new Error(`WORK_ACCESS_GRANT_ENDPOINT_UNKNOWN:${endpointGrant.endpointId}`)
      expect(endpointGrant).toMatchObject({
        accessGrantId: accessGrant.id,
        endpointId: endpoint.id,
        endpointRevision: endpoint.revision,
        protocol: endpoint.protocol,
        action: 'connect',
        health: 'healthy',
        expiresAt: expect.any(String),
      })
      if (endpoint.protocol === 'http' || endpoint.protocol === 'https') {
        expect(endpointGrant.connectUrl).toBe(`/connect/${endpointGrant.id}/`)
      }
    }
    if (REAL_WORK_VM) {
      const vmLicense = await readRealWorkVmLicenseStatus(sshEndpointGrant, vmSshIdentity)
      expect(vmLicense).toMatchObject({
        driverVersion: expect.any(String),
        licenseStatus: 'Licensed',
      })
      const vmResult = await runRealWorkVmCudaProbe(sshEndpointGrant, vmSshIdentity)
      expect(vmResult).toEqual({ count: 256, sum: 32640, max: 255 })
    } else {
      const httpGrant = accessGrant.endpointGrants.find(
        (endpointGrant) => (endpointGrant.protocol === 'http' || endpointGrant.protocol === 'https')
          && typeof endpointGrant.connectUrl === 'string',
      )
      if (!httpGrant?.connectUrl) throw new Error('WORK_ACCESS_GRANT_HTTP_CONNECTION_MISSING')
      const runtimeResponse = await page.request.get(httpGrant.connectUrl)
      const runtimeBody = await runtimeResponse.text()
      if (!runtimeResponse.ok()) {
        throw new Error(`WORK_ACCESS_GRANT_RUNTIME_GET_FAILED:${runtimeResponse.status()}:${runtimeBody.slice(0, 2000)}`)
      }
      expect(runtimeResponse.status()).toBe(200)
      if (REAL_WORK_MODE) {
        const seedBody = await readWorkFile(
          page.request,
          httpGrant.connectUrl,
          'seed.txt',
          'REAL_WORK_SEED_FILE_READ_FAILED',
        )
        expect(seedBody.trim()).toBe(expectedSeedMarker)
      } else {
      expect(runtimeBody).toContain('Welcome to nginx')
      }
    }
    if (REAL_WORK_GPU && !REAL_WORK_VM) {
      const terminalPage = await page.context().newPage()
      try {
        expect(await runTerminalCudaProbe(terminalPage, project.id, environmentId)).toEqual({ count: 256, sum: 32640, max: 255 })
      } finally {
        await terminalPage.close()
      }
    }
    if (REAL_WORK_VM) {
      const revokedGrant = accessGrant
      const revokedEndpointGrant = sshEndpointGrant
      vmSshSession = await openPinnedSshSession(revokedEndpointGrant, vmSshIdentity)
      if (vmSshSession.closed) throw new Error('WORK_SSH_SESSION_CLOSED_BEFORE_REVOKE')

      const revokedAccessGrant = await revokeEnvironmentAccessGrantByUi(
        page,
        project.id,
        environmentId,
        revokedGrant.id,
      )
      expect(revokedAccessGrant).toMatchObject({
        id: revokedGrant.id,
        state: 'revoked',
        reasonCode: 'user_revoked',
      })
      const closedSession = await vmSshSession.waitForClose()
      vmSshSession = null
      if (
        !closedSession.closed
        || closedSession.code === null
        || closedSession.signal !== null
        || !['remote_terminated', 'process_exited', 'process_failed'].includes(closedSession.reasonCode)
      ) {
        throw new Error('WORK_SSH_SESSION_CLOSE_RESULT_INVALID')
      }

      await expect(
        runPinnedSsh(revokedEndpointGrant, vmSshIdentity, 'printf WORK_SSH_OLD_ALIAS_ACCEPTED'),
      ).rejects.toThrow(/^WORK_SSH_COMMAND_FAILED:(?:\d+|signal)$/)

      const reissued = await issueWorkAccessGrantByUi(page, project.id, environment)
      expect(reissued.grant.id).not.toBe(revokedGrant.id)
      accessGrant = reissued.grant
      expectedAccessGrantId = accessGrant.id
      sshEndpointGrant = reissued.endpointGrant
      revocationTargetGrant = accessGrant
      const reissuedVmLicense = await readRealWorkVmLicenseStatus(sshEndpointGrant, vmSshIdentity)
      expect(reissuedVmLicense).toMatchObject({
        driverVersion: expect.any(String),
        licenseStatus: 'Licensed',
      })
      const reissuedCudaResult = await runRealWorkVmCudaProbe(sshEndpointGrant, vmSshIdentity)
      expect(reissuedCudaResult).toEqual({ count: 256, sum: 32640, max: 255 })
    }
    await assertNoStuckProgress(page, 'student-work-environment')
    await auditAccessibility(page, 'student-work-environment')
    guards.assertCleanConsole('student-work-environment')

    await page.goto(`/researcher/software?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
    await selectProjectByUi(page, project.id)
    await expect(page.getByRole('button', { name: '配置现有 Work', exact: true })).toHaveAttribute('aria-pressed', 'true')
    const workEnvironment = page.getByLabel('Work 环境')
    await expect(workEnvironment.locator(`option[value="${environmentId}"]`)).toHaveCount(1, { timeout: 120_000 })
    await workEnvironment.selectOption(environmentId)
    await page.getByLabel('材料包 ID').fill(packageData.id)
    await page.getByLabel('材料包 Revision').fill(String(packageData.revision))
    await page.getByLabel(/我确认 Agent 可能修改该 Work/).check()
    const planResponsePromise = page.waitForResponse(
      (response) => {
        const url = new URL(response.url())
        return response.request().method() === 'GET'
          && url.pathname.startsWith(`/api/v1/projects/${project.id}/agent-runs/`)
          && url.pathname.endsWith('/work-configuration/plan')
      },
      { timeout: 300_000 },
    )
    const configurationResponsePromise = page.waitForResponse(
      (response) => {
        const url = new URL(response.url())
        return response.request().method() === 'POST'
          && url.pathname === `/api/v1/projects/${project.id}/work-configuration-runs`
      },
      { timeout: 300_000 },
    )
    await page.getByRole('button', { name: '生成 Work 配置', exact: true }).click()
    const configurationResponse = await configurationResponsePromise
    const configurationRun = await expectJson(configurationResponse, 'WORK_CONFIGURATION_RUN_CREATE_FAILED')
    expect(configurationRun).toMatchObject({ id: expect.any(String), projectId: project.id })
    const configurationRequestBody = configurationResponse.request().postDataJSON()
    expect(configurationRequestBody).toMatchObject({
      projectId: project.id,
      packageId: packageData.id,
      packageRevision: packageData.revision,
      environmentId,
      environmentRevision: expect.any(Number),
      policyId: expect.any(String),
      policyRevision: expect.any(Number),
    })
    if (
      !Number.isInteger(configurationRequestBody.environmentRevision)
      || configurationRequestBody.environmentRevision < 1
      || !Number.isInteger(configurationRequestBody.policyRevision)
      || configurationRequestBody.policyRevision < 1
    ) {
      throw new Error('WORK_CONFIGURATION_REQUEST_SCOPE_INVALID')
    }
    const approvedConfigurationResourceRequestIds = new Set()
    const configurationRunStatus = await pollJson(
      page.request,
      `/api/v1/projects/${project.id}/agent-runs/${configurationRun.id}`,
      async (value) => {
        await approvePendingAgentTaskResourceByUi(adminPage, {
          projectId: project.id,
          run: value,
          runId: configurationRun.id,
          packageId: packageData.id,
          policyId: configurationRequestBody.policyId,
          policyRevision: configurationRequestBody.policyRevision,
          studentActorId,
          trackKind: 'work_configuration',
          purposeKind: 'work_configuration',
          environmentId: configurationRequestBody.environmentId,
          environmentRevision: configurationRequestBody.environmentRevision,
          approvedRequestIds: approvedConfigurationResourceRequestIds,
        })
        return value.state === 'awaiting_approval' || terminalRunState(value.state)
      },
      'WORK_CONFIGURATION_RUN_STATUS_FAILED',
      300_000,
    )
    if (configurationRunStatus.state !== 'awaiting_approval') {
      throw new Error(`WORK_CONFIGURATION_RUN_FAILED_BEFORE_PLAN:${configurationRunStatus.state}`)
    }
    const configurationPlan = await expectJson(await planResponsePromise, 'WORK_CONFIGURATION_PLAN_LOAD_FAILED')
    expect(configurationPlan).toMatchObject({
      plan: {
        environmentId,
        environmentRevision: expect.any(Number),
        id: expect.any(String),
        requiresRestart: expect.any(Boolean),
        revision: expect.any(Number),
      },
      scriptContent: expect.any(String),
    })
    if (REAL_WORK_MODE) {
      expect(configurationPlan.scriptContent).toContain(expectedPersistenceMarker)
      expect(configurationPlan.verificationScriptContent).toContain('persistence-marker.txt')
    }
    await expect(page.getByRole('heading', { name: 'Work 配置计划审核', exact: true })).toBeVisible({ timeout: 300_000 })
    await expect(page.locator('.plan-code').getByRole('heading', { name: '配置脚本', exact: true, level: 5 })).toBeVisible()
    await page.getByLabel('批准原因').fill('已审阅 Work 配置脚本及目标环境，允许执行。')
    const restartConfirmation = page.getByLabel(/我确认执行前后 Work 环境会重启/)
    if (configurationPlan.plan.requiresRestart) {
      await expect(restartConfirmation).toBeVisible()
      await restartConfirmation.check()
    } else {
      await expect(restartConfirmation).toHaveCount(0)
    }
    const approveConfigurationResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${project.id}/agent-runs/${configurationRun.id}/work-configuration/approve`
    })
    await page.getByRole('button', { name: '批准并执行 Work 配置', exact: true }).click()
    const approveConfigurationResponse = await approveConfigurationResponsePromise
    const approvedRun = await expectJson(approveConfigurationResponse, 'WORK_CONFIGURATION_APPROVAL_FAILED')
    expect(approvedRun).toMatchObject({ id: configurationRun.id, projectId: project.id })
    const completedConfigurationRun = await pollJson(
      page.request,
      `/api/v1/projects/${project.id}/agent-runs/${configurationRun.id}`,
      (value) => terminalRunState(value.state),
      'WORK_CONFIGURATION_RUN_STATUS_FAILED',
      300_000,
    )
    if (completedConfigurationRun.state !== 'succeeded') throw new Error(`WORK_CONFIGURATION_RUN_FAILED:${completedConfigurationRun.state}`)

    if (REAL_WORK_MODE) {
      const configuredEnvironment = await waitForEnvironment(page.request, environmentId, 'ready')
      if (REAL_WORK_VM) {
        const configuredConnection = await issueWorkAccessGrantByUi(page, project.id, configuredEnvironment)
        revocationTargetGrant = configuredConnection.grant
        sshEndpointGrant = configuredConnection.endpointGrant
        const configuredPersistenceBody = await readRealWorkVmWorkspaceFile(
          configuredConnection.endpointGrant,
          vmSshIdentity,
          VM_PERSISTENCE_MARKER_PATH,
        )
        expect(configuredPersistenceBody.trim()).toBe(expectedPersistenceMarker)
      } else {
        const existingGrant = await expectJson(
          await page.request.get(`/api/v1/access-grants/${accessGrant.id}`),
          'REAL_WORK_ACCESS_GRANT_READ_AFTER_CONFIGURATION_FAILED',
        )
        let configuredConnection
        if (existingGrant.state === 'active' && existingGrant.environmentRevision === configuredEnvironment.revision) {
          configuredConnection = { grant: existingGrant, httpGrant: httpEndpointGrant(existingGrant) }
        } else {
          configuredConnection = await issueWorkAccessGrant(page, project.id, configuredEnvironment)
          revocationTargetGrant = configuredConnection.grant
        }
        const configuredBody = await readWorkEndpoint(
          page.request,
          configuredConnection.httpGrant.connectUrl,
          'REAL_WORK_CONFIGURED_ENDPOINT_READ_FAILED',
        )
        expect(configuredBody).toContain('seed.txt')
        const configuredSeedBody = await readWorkFile(
          page.request,
          configuredConnection.httpGrant.connectUrl,
          'seed.txt',
          'REAL_WORK_CONFIGURED_SEED_FILE_READ_FAILED',
        )
        const configuredPersistenceBody = await readWorkFile(
          page.request,
          configuredConnection.httpGrant.connectUrl,
          'persistence-marker.txt',
          'REAL_WORK_CONFIGURED_PERSISTENCE_FILE_READ_FAILED',
        )
        expect(configuredSeedBody.trim()).toBe(expectedSeedMarker)
        expect(configuredPersistenceBody.trim()).toBe(expectedPersistenceMarker)
      }

      await page.goto(`/researcher/environments?projectId=${encodeURIComponent(project.id)}&environmentId=${encodeURIComponent(environmentId)}`, { waitUntil: 'domcontentloaded' })
      const restartButton = page.getByRole('button', { name: '重启', exact: true })
      await expect(restartButton).toBeEnabled({ timeout: 120_000 })
      const restartResponsePromise = page.waitForResponse((response) => {
        const url = new URL(response.url())
        return response.request().method() === 'POST' && url.pathname === `/api/v1/environments/${environmentId}/restart`
      })
      await restartButton.click()
      const restartAccepted = await expectJson(await restartResponsePromise, 'REAL_WORK_ENVIRONMENT_RESTART_FAILED')
      expect(restartAccepted).toMatchObject({
        environmentId,
        operationId: expect.any(String),
        revision: expect.any(Number),
        statusUrl: expect.stringContaining(`/api/v1/environments/${environmentId}/operations/`),
      })
      const restartOperation = await pollJson(
        page.request,
        restartAccepted.statusUrl,
        (value) => ['succeeded', 'failed', 'cancelled'].includes(value.state),
        'REAL_WORK_ENVIRONMENT_RESTART_OPERATION_STATUS_FAILED',
        240_000,
      )
      expect(restartOperation).toMatchObject({
        environmentId,
        operationId: restartAccepted.operationId,
        kind: 'restart',
        state: 'succeeded',
      })
      const restartedEnvironment = await waitForEnvironment(page.request, environmentId, 'ready')
      if (REAL_WORK_VM) {
        const restartedConnection = await issueWorkAccessGrantByUi(page, project.id, restartedEnvironment)
        revocationTargetGrant = restartedConnection.grant
        sshEndpointGrant = restartedConnection.endpointGrant
        const restartedPersistenceBody = await readRealWorkVmWorkspaceFile(
          restartedConnection.endpointGrant,
          vmSshIdentity,
          VM_PERSISTENCE_MARKER_PATH,
        )
        expect(restartedPersistenceBody.trim()).toBe(expectedPersistenceMarker)
        const restartedVmLicense = await readRealWorkVmLicenseStatus(restartedConnection.endpointGrant, vmSshIdentity)
        expect(restartedVmLicense).toMatchObject({
          driverVersion: expect.any(String),
          licenseStatus: 'Licensed',
        })
        const restartedCudaResult = await runRealWorkVmCudaProbe(restartedConnection.endpointGrant, vmSshIdentity)
        expect(restartedCudaResult).toEqual({ count: 256, sum: 32640, max: 255 })
      } else {
        const restartedConnection = await issueWorkAccessGrant(page, project.id, restartedEnvironment)
        revocationTargetGrant = restartedConnection.grant
        const restartedSeedBody = await readWorkFile(
          page.request,
          restartedConnection.httpGrant.connectUrl,
          'seed.txt',
          'REAL_WORK_RESTARTED_SEED_FILE_READ_FAILED',
        )
        const restartedPersistenceBody = await readWorkFile(
          page.request,
          restartedConnection.httpGrant.connectUrl,
          'persistence-marker.txt',
          'REAL_WORK_RESTARTED_PERSISTENCE_FILE_READ_FAILED',
        )
        expect(restartedSeedBody.trim()).toBe(expectedSeedMarker)
        expect(restartedPersistenceBody.trim()).toBe(expectedPersistenceMarker)
        if (REAL_WORK_GPU && !REAL_WORK_VM) {
          const terminalPage = await page.context().newPage()
          try {
            expect(await runTerminalCudaProbe(terminalPage, project.id, environmentId)).toEqual({ count: 256, sum: 32640, max: 255 })
          } finally {
            await terminalPage.close()
          }
        }
      }
    }

    await page.goto(`/researcher/resources?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
    await selectProjectByUi(page, project.id)
    // The resources page lists every project lease, including the authoring
    // config sandbox's task lease, so the FIRST 续期 button is not necessarily
    // this Work environment's. Scope to the lease row that links into the
    // environments console (only environment-targeted leases carry it).
    const workLeaseRenewButton = page
      .locator('li.resource-row')
      .filter({ has: page.locator('a[href*="/researcher/environments"]') })
      .getByRole('button', { name: '续期', exact: true })
    await expect(workLeaseRenewButton).toBeVisible({ timeout: 120_000 })
    const renewResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST' && url.pathname === `/api/v1/resource-leases/${lease.id}/renew`
    })
    await workLeaseRenewButton.click()
    const renewedLease = await expectJson(await renewResponsePromise, 'RESOURCE_LEASE_RENEW_FAILED')
    expect(renewedLease).toMatchObject({ id: lease.id, state: 'active', revision: expect.any(Number) })
    expect(new Date(renewedLease.expiresAt).getTime()).toBeGreaterThan(new Date(lease.expiresAt).getTime())
    await assertNoStuckProgress(page, 'researcher-resource-lease')
    await auditAccessibility(page, 'researcher-resource-lease')

    await page.goto(`/researcher/environments?projectId=${encodeURIComponent(project.id)}&environmentId=${encodeURIComponent(environmentId)}`, { waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('button', { name: '停止', exact: true })).toBeEnabled({ timeout: 120_000 })
    const stopResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST' && url.pathname === `/api/v1/environments/${environmentId}/stop`
    })
    await page.getByRole('button', { name: '停止', exact: true }).click()
    const stopAccepted = await expectJson(await stopResponsePromise, 'WORK_ENVIRONMENT_STOP_FAILED')
    expect(stopAccepted).toMatchObject({
      environmentId,
      operationId: expect.any(String),
      revision: expect.any(Number),
      statusUrl: expect.stringContaining(`/api/v1/environments/${environmentId}/operations/`),
    })
    const stopOperation = await pollJson(
      page.request,
      stopAccepted.statusUrl,
      (value) => ['succeeded', 'failed', 'cancelled'].includes(value.state),
      'WORK_ENVIRONMENT_STOP_OPERATION_STATUS_FAILED',
      240_000,
    )
    expect(stopOperation).toMatchObject({
      environmentId,
      operationId: stopAccepted.operationId,
      kind: 'stop',
      state: 'succeeded',
    })
    const operationList = await expectJson(
      await page.request.get(`/api/v1/environments/${environmentId}/operations`),
      'WORK_ENVIRONMENT_OPERATIONS_LIST_FAILED',
    )
    expect(operationList).toMatchObject({
      items: expect.any(Array),
      snapshotAt: expect.any(String),
      snapshotSequence: expect.any(String),
    })
    const listedStopOperation = operationList.items.find((item) => item.operationId === stopAccepted.operationId)
    expect(listedStopOperation).toMatchObject({
      environmentId,
      operationId: stopAccepted.operationId,
      kind: 'stop',
      state: 'succeeded',
    })
    await page.reload({ waitUntil: 'domcontentloaded' })
    const operationsTab = page.getByRole('button', { name: '异步操作与诊断', exact: true })
    await expect(operationsTab).toBeVisible({ timeout: 120_000 })
    await operationsTab.click()
    const operationTimelineHeading = page.getByRole('heading', { name: '操作与诊断时间线', exact: true })
    await expect(operationTimelineHeading).toBeVisible({ timeout: 120_000 })
    const operationPane = page.locator('.tab-pane').filter({ has: operationTimelineHeading })
    await expect(operationPane.getByRole('alert')).toHaveCount(0)
    const stoppedEnvironment = await waitForEnvironment(page.request, environmentId, 'stopped')
    const revokedAccessGrant = await pollJson(
      page.request,
      `/api/v1/access-grants/${revocationTargetGrant.id}`,
      (value) => ['revoked', 'expired', 'denied'].includes(value.state),
      'WORK_ACCESS_GRANT_REVOKE_STATUS_FAILED',
      120_000,
    )
    expect(revokedAccessGrant).toMatchObject({
      id: revocationTargetGrant.id,
      state: 'revoked',
      reasonCode: 'environment_stopped',
    })

    await page.goto(`/researcher/resources?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
    await selectProjectByUi(page, project.id)
    const workLeaseReclaimButton = page
      .locator('li.resource-row')
      .filter({ has: page.locator('a[href*="/researcher/environments"]') })
      .getByRole('button', { name: '回收', exact: true })
    await expect(workLeaseReclaimButton).toBeVisible({ timeout: 120_000 })
    const reclaimResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST' && url.pathname === `/api/v1/resource-leases/${lease.id}/revoke`
    })
    await workLeaseReclaimButton.click()
    // 回收 opens the same confirm dialog as the admin release helper; the
    // revoke POST only fires after the confirm action.
    const reclaimDialog = page.locator('dialog.confirm-dialog[role="alertdialog"]')
    await expect(reclaimDialog).toBeVisible()
    await reclaimDialog.locator('.filled-button').click()
    const revokedLease = await expectJson(await reclaimResponsePromise, 'RESOURCE_LEASE_RECLAIM_FAILED')
    expect(revokedLease).toMatchObject({ id: lease.id })
    const finalLease = await pollJson(
      page.request,
      `/api/v1/resource-leases/${lease.id}`,
      (value) => ['revoked', 'expired'].includes(value.state),
      'RESOURCE_LEASE_RECLAIM_STATUS_FAILED',
      240_000,
    )
    if (finalLease.state !== 'revoked') throw new Error(`RESOURCE_LEASE_NOT_REVOKED:${finalLease.state}`)
    await waitForDeletedEnvironment(page.request, environmentId)
    if (vmSshKey) {
      await deleteStudentSshKeyByUi(page, vmSshKey)
      vmSshKey = null
    }
    await assertNoStuckProgress(page, 'researcher-resource-released')
    await auditAccessibility(page, 'researcher-resource-released')
    guards.assertCleanConsole('researcher-resource-released')
    if (REAL_WORK_MODE) {
      const finance = await waitForSettledWorkUsageCharges(browser, baseURL, {
        projectId: project.id,
        leases: [{ requestId: trackedRequestId, leaseId: trackedLeaseId }],
        baselineChargeIds,
        gpu: REAL_WORK_GPU ? { ...REAL_WORK_GPU, rate: gpuRate } : null,
      })
      if (EXISTING_PROJECT_MODE) {
        const finalPolicy = await readExistingProjectPolicy(page.request, project.id)
        expect(finalPolicy.budget).toEqual(existingProjectPolicy.budget)
        const finalResourceBudget = await readExistingProjectResourceBudget(
          adminPage.request,
          project.id,
          'REAL_WORK_EXISTING_PROJECT_RESOURCE_BUDGET_FINAL_READ_FAILED',
        )
        if (existingResourceBudget === null) {
          expect(finalResourceBudget).toBeNull()
        } else {
          expect(finalResourceBudget).toMatchObject({
            projectId: project.id,
            limit: existingResourceBudget.limit,
            warningAt: existingResourceBudget.warningAt,
          })
        }
      }
      await inspectRealWorkFinanceByUi(browser, baseURL, project.id, {
        gpu: REAL_WORK_GPU,
        usageRecordIds: finance.matches.map(({ usage }) => usage.id),
        expectedCharges: finance.matches.map(({ charge }) => charge),
        requireBudget: !EXISTING_PROJECT_MODE || existingResourceBudget !== null,
      })
      if (!EXISTING_PROJECT_MODE && !REAL_WORK_RESUME) {
        const settledCharge = selectRealWorkFinanceAdjustmentCharge(finance.matches)
        await verifyRealWorkFinanceAdjustmentByUi(browser, baseURL, project.id, settledCharge)
      }
    }
    await assertConnectionBlockedAfterLeaseRevoke(page, project.id, stoppedEnvironment)
  } catch (error) {
    primaryFailure = error
    if (REAL_WORK_MODE) {
      try {
        await cleanupWorkResources(page.request, baseURL, project.id, trackedEnvironmentId, trackedLeaseId, trackedRequestId, page)
      } catch (cleanupError) {
        const primaryMessage = error instanceof Error ? error.message : String(error)
        const cleanupMessage = cleanupError instanceof Error ? cleanupError.message : String(cleanupError)
        primaryFailure = new Error(`REAL_WORK_PRIMARY_FAILURE:${primaryMessage};REAL_WORK_CLEANUP_FAILED:${cleanupMessage}`, { cause: error })
      }
    }
  } finally {
    try {
      await vmSshSession?.close()
    } catch (error) {
      cleanupFailures.push(error)
    }
    try {
      if (vmSshKey) await deleteStudentSshKeyByUi(page, vmSshKey)
    } catch (error) {
      cleanupFailures.push(error)
    }
    try {
      await packageCopy?.cleanup()
    } catch (error) {
      cleanupFailures.push(error)
    }
    try {
      await vmSshIdentity?.cleanup()
    } catch (error) {
      cleanupFailures.push(error)
    }
    try {
      await adminContext?.close()
    } catch (error) {
      cleanupFailures.push(error)
    }
  }
  if (primaryFailure && cleanupFailures.length > 0) {
    const primaryMessage = primaryFailure instanceof Error ? primaryFailure.message : String(primaryFailure)
    const cleanupMessages = cleanupFailures.map((error) => error instanceof Error ? error.message : String(error)).join(';')
    throw new Error(`REAL_WORK_PRIMARY_FAILURE:${primaryMessage};REAL_WORK_FINAL_CLEANUP_FAILED:${cleanupMessages}`, { cause: primaryFailure })
  }
  if (primaryFailure) throw primaryFailure
  if (cleanupFailures.length > 0) {
    const cleanupMessages = cleanupFailures.map((error) => error instanceof Error ? error.message : String(error)).join(';')
    throw new Error(`REAL_WORK_FINAL_CLEANUP_FAILED:${cleanupMessages}`, { cause: cleanupFailures[0] })
  }
})
