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
  addProjectStudentByUi,
  readActorId,
  snapshotProjectResourceRequestIds,
  startExperimentRunByUi,
  uploadPackageDirectoryByUi,
  waitForFrozenSubmission,
  waitForProjectEvaluationResultWithResourceApproval,
} from '../support/real-experiment.mjs'
import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { assertNoStuckProgress, auditAccessibility, installUsabilityGuards } from '../support/usability.mjs'
import { approveResourceRequestByUi } from '../support/real-resource.mjs'
import {
  inspectRealWorkFinanceByUi,
  readResourceRates,
  waitForSettledExperimentUsageCharges,
} from '../support/real-work.mjs'
import { issueAccessGrantAndConnect, hasTerminalLine, typeTerminalCommand } from '../support/real-gpu.mjs'
import { deleteEnvironmentByUi, stopEnvironmentByUi } from '../support/environment-lifecycle.mjs'
import { issueEnvironmentAccessGrantByUi, revokeEnvironmentAccessGrantByUi } from '../support/ssh-access.mjs'

const LAB_ROOT = fileURLToPath(new URL('../../../examples', import.meta.url))

const LABS = Object.freeze({
  xv6: Object.freeze({
    root: join(LAB_ROOT, 'xv6-lab'),
    frozenPath: 'student/student.c',
    starterOutput: 'xv6-starter: fix me',
    fixCommands: [
      "sed -i 's/xv6-starter: fix me/xv6-student: hello/' student/student.c",
    ],
    expectImprovement: true,
  }),
  cuda: Object.freeze({
    root: join(LAB_ROOT, 'cuda-lab'),
    frozenPath: 'student/gpu_stats.cu',
    gpuMode: process.env.LABWEAVER_E2E_GPU_MODE?.trim() || 'exclusive',
    gpuClass: process.env.LABWEAVER_E2E_GPU_CLASS?.trim() || 'nvidia-cuda',
    // The starter launches too few threads. The student corrects the coverage
    // and captures the statistics from a real GPU run in the GPU environment.
    fixCommands: [
      "sed -i 's/#define BLOCKS 2/#define BLOCKS 8/' student/gpu_stats.cu",
      '/usr/local/cuda/bin/nvcc -o gpu_stats student/gpu_stats.cu && ./gpu_stats > student/result.txt && grep -Fx \'N=256\' student/result.txt && grep -Fx \'sum=32640\' student/result.txt && grep -Fx \'max=255\' student/result.txt',
    ],
    expectImprovement: true,
  }),
  ctf: Object.freeze({
    root: join(LAB_ROOT, 'ctf-web-lab'),
    frozenPath: 'student/flag.txt',
    uploadCheckPath: 'statement.md',
    kind: 'ctf',
    expectImprovement: true,
  }),
})

const LAB = LABS[process.env.LABWEAVER_E2E_LAB ?? '']
const RETAIN_CUDA_SAMPLE = process.env.LABWEAVER_E2E_RETAIN_CUDA_SAMPLE?.trim() === '1'
if (RETAIN_CUDA_SAMPLE && process.env.LABWEAVER_E2E_LAB !== 'cuda') {
  throw new Error('LAB_EXPERIMENT_RETAIN_CUDA_SAMPLE_REQUIRES_CUDA_LAB')
}
if (RETAIN_CUDA_SAMPLE && LAB?.gpuMode !== 'exclusive') {
  throw new Error('LAB_EXPERIMENT_RETAIN_CUDA_SAMPLE_REQUIRES_EXCLUSIVE_GPU')
}
// This journey owns real projects, environments and charges. Keep separate
// ceilings for authoring and image build so the four-hour test budget leaves
// time for publication, evaluation, finance and cleanup.
const FULL_CHAIN_TIMEOUT_MS = 14_400_000
const AUTHORING_RUN_TIMEOUT_MS = Number(process.env.LABWEAVER_E2E_AGENT_RUN_TIMEOUT_MS) || 9_000_000
const CANDIDATE_BUILD_TIMEOUT_MS = 3_600_000
const AUTHORING_RESOURCE_PROVIDER_BINDING =
  process.env.LABWEAVER_E2E_AUTHORING_PROVIDER_BINDING?.trim()
  || process.env.LABWEAVER_E2E_PROVIDER_BINDING?.trim()
  || 'container-primary-v1'

test.skip(!LAB, 'Set LABWEAVER_E2E_LAB=xv6, LABWEAVER_E2E_LAB=cuda, or LABWEAVER_E2E_LAB=ctf for a real lab acceptance run.')
test.describe.configure({ timeout: FULL_CHAIN_TIMEOUT_MS, retries: 0 })

function diagnosticCodes(run) {
  return run.tracks
    .flatMap((track) => track.attempts ?? [])
    .map((attempt) => attempt.diagnosticCode)
    .filter(Boolean)
    .join(',') || 'no attempt diagnostic'
}

function terminalRunState(value) {
  return ['succeeded', 'partially_succeeded', 'failed', 'cancelled'].includes(value)
}

function runHasActiveAttempt(run) {
  const activeStates = new Set(['pending', 'running', 'repairing', 'awaiting_approval'])
  return run.tracks.some((track) => (track.attempts ?? []).some((attempt) => activeStates.has(attempt.state)))
}

function runIsFullyTerminal(run) {
  return terminalRunState(run.state) && !runHasActiveAttempt(run)
}

function runHasManualRetry(run) {
  return run.tracks.some((track) => (track.attempts ?? []).some((attempt) => (
    Number.isInteger(attempt.number) && attempt.number >= 2
  )))
}

async function configureLabPackageCopy(packageCopy) {
  if (process.env.LABWEAVER_E2E_LAB !== 'cuda') return
  if (!/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(LAB.gpuClass)) {
    throw new Error(`LAB_EXPERIMENT_GPU_CLASS_INVALID:${LAB.gpuClass}`)
  }
  const environmentPath = join(packageCopy, 'environment.yaml')
  const environment = await readFile(environmentPath, 'utf8')
  const lines = environment.split(/\r?\n/)
  const gpuIndex = lines.findIndex((line) => line.trim() === 'gpu:')
  const classIndex = gpuIndex >= 0
    ? lines.findIndex((line, index) => index > gpuIndex && index <= gpuIndex + 3 && line.trimStart().startsWith('class:'))
    : -1
  if (gpuIndex < 0 || classIndex < 0) throw new Error('LAB_EXPERIMENT_GPU_CLASS_FIELD_MISSING')
  const indent = lines[classIndex].match(/^\s*/)?.[0] ?? ''
  lines[classIndex] = `${indent}class: ${LAB.gpuClass}`
  if (RETAIN_CUDA_SAMPLE) {
    const retentionIndex = lines.findIndex((line) => line.trim() === 'retention:')
    const retainUntilIndex = retentionIndex >= 0
      ? lines.findIndex((line, index) => index > retentionIndex && line.trimStart().startsWith('retainUntil:'))
      : -1
    const dispositionIndex = retentionIndex >= 0
      ? lines.findIndex((line, index) => index > retentionIndex && line.trimStart().startsWith('disposition:'))
      : -1
    if (retentionIndex < 0 || retainUntilIndex < 0 || dispositionIndex < 0) {
      throw new Error('LAB_EXPERIMENT_CUDA_PERMANENT_RETENTION_FIELDS_MISSING')
    }
    const retainUntilIndent = lines[retainUntilIndex].match(/^\s*/)?.[0] ?? ''
    const dispositionIndent = lines[dispositionIndex].match(/^\s*/)?.[0] ?? ''
    lines[retainUntilIndex] = `${retainUntilIndent}retainUntil: null`
    lines[dispositionIndex] = `${dispositionIndent}disposition: retain_until_revoked`
  }
  const updatedEnvironment = lines.join('\n')
  await writeFile(environmentPath, updatedEnvironment, 'utf8')

  const manifestPath = join(packageCopy, 'manifest.json')
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'))
  const manifestFile = manifest.spec?.files?.find((file) => file.path === 'environment.yaml')
  if (!manifestFile) throw new Error('LAB_EXPERIMENT_GPU_MANIFEST_ENTRY_MISSING')
  manifestFile.sha256 = createHash('sha256').update(updatedEnvironment).digest('hex')
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8')
}

async function selectPermanentMaterialRetentionByUi(page) {
  const choice = page.getByRole('radio', { name: '不过期，直到明确撤回', exact: true })
  await expect(choice).toBeVisible()
  await choice.check()
  await expect(choice).toBeChecked()
}

async function waitForExperimentRun(request, projectId, runId, onPendingResourceRequests = null, waitOptions = {}) {
  const run = await waitForTerminalExperimentRun(request, projectId, runId, onPendingResourceRequests, waitOptions)
  if (run.state !== 'succeeded') {
    throw new Error(`LAB_EXPERIMENT_AGENT_RUN_FAILED:${run.state}:${diagnosticCodes(run)}`)
  }
  const environment = run.tracks.find((track) => track.kind === 'environment')
  const evaluation = run.tracks.find((track) => track.kind === 'evaluation')
  if (!environment?.candidateId || !evaluation?.candidateId) {
    throw new Error('LAB_EXPERIMENT_CANDIDATES_MISSING')
  }
  return { run, environmentCandidateId: environment.candidateId, evaluationCandidateId: evaluation.candidateId }
}

async function waitForTerminalExperimentRun(
  request,
  projectId,
  runId,
  onPendingResourceRequests = null,
  { minimumRevision = 0, requiredTrackKind = 'environment', requiredTrackAttemptNumber = null } = {},
) {
  return await pollJson(
    request,
    `/api/v1/projects/${projectId}/agent-runs/${runId}`,
    async (value) => {
      const revisionIsFresh = Number.isInteger(value.revision) && value.revision >= minimumRevision
      const requiredAttempt = requiredTrackAttemptNumber === null
        ? null
        : value.tracks.find((track) => track.kind === requiredTrackKind)?.attempts
          ?.find((attempt) => attempt.number === requiredTrackAttemptNumber)
      const requiredAttemptIsTerminal = requiredTrackAttemptNumber === null
        || ['succeeded', 'failed', 'cancelled'].includes(requiredAttempt?.state)
      const complete = runIsFullyTerminal(value) && revisionIsFresh && requiredAttemptIsTerminal
      if (!complete) await onPendingResourceRequests?.()
      return complete
    },
    'LAB_EXPERIMENT_AGENT_RUN_STATUS_FAILED',
    AUTHORING_RUN_TIMEOUT_MS,
  )
}

function isFirstFailedTrackAttempt(run, trackKind) {
  if (run.state !== 'failed' && run.state !== 'partially_succeeded') return false
  if (runHasActiveAttempt(run)) return false
  const track = run.tracks.find((item) => item.kind === trackKind)
  const attempts = track?.attempts ?? []
  const last = attempts[attempts.length - 1]
  return attempts.length === 1
    && last?.number === 1
    && (last.state === 'failed' || last.state === 'cancelled')
}

async function retryFailedTrackByUi(page, projectId, run, trackKind) {
  if (runHasManualRetry(run)) throw new Error('LAB_EXPERIMENT_RESUME_MANUAL_RETRY_ALREADY_USED')
  if (!isFirstFailedTrackAttempt(run, trackKind)) {
    throw new Error(`LAB_EXPERIMENT_RESUME_${trackKind.toUpperCase()}_RETRY_TARGET_INVALID`)
  }
  const label = trackKind === 'environment' ? '环境' : '评测'
  await page.goto(
    `/teacher/materials?projectId=${encodeURIComponent(projectId)}&packageId=${encodeURIComponent(run.packageId)}&runId=${encodeURIComponent(run.id)}`,
    { waitUntil: 'domcontentloaded' },
  )
  await selectProjectByUi(page, projectId)
  const retry = page.getByRole('button', { name: `重试${label}轨道`, exact: true })
  await expect(retry).toBeVisible({ timeout: 120_000 })
  await expect(retry).toBeEnabled()

  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/projects/${projectId}/agent-runs/${run.id}/tracks/${trackKind}/retry`
  })
  await retry.click()
  const response = await responsePromise
  const requestHeaders = response.request().headers()
  expect(requestHeaders['idempotency-key']).toMatch(/^[0-9a-f-]{36}$/i)
  expect(requestHeaders['if-match']).toBe(`"rev-${run.revision}"`)
  const accepted = await expectJson(response, 'LAB_EXPERIMENT_ENVIRONMENT_RETRY_FAILED')
  expect(accepted).toMatchObject({ id: run.id, projectId })
  expect(accepted.revision).toBeGreaterThan(run.revision)
  return accepted
}

async function approveAuthoringResourceRequestsByUi(adminPage, projectId, runId, requesterId) {
  const response = await adminPage.request.get(`/api/v1/projects/${projectId}/resource-requests`)
  const requests = await expectJson(response, 'LAB_EXPERIMENT_AUTHORING_RESOURCE_REQUESTS_READ_FAILED')
  if (!Array.isArray(requests)) throw new Error('LAB_EXPERIMENT_AUTHORING_RESOURCE_REQUESTS_INVALID')

  const prefix = `authoring-${runId.replaceAll('-', '')}-`
  const pending = requests.filter((item) => (
    item?.projectId === projectId
    && item?.requesterId === requesterId
    && item?.target?.kind === 'task'
    && typeof item.requestKey === 'string'
    && item.requestKey.startsWith(prefix)
    && item.state === 'reviewing'
  ))

  for (const request of pending) {
    if (!Number.isInteger(request.requestedDurationSeconds) || request.requestedDurationSeconds <= 0) {
      throw new Error(`LAB_EXPERIMENT_AUTHORING_RESOURCE_DURATION_INVALID:${request.id ?? 'missing'}`)
    }
    await approveResourceRequestByUi(adminPage, {
      requestKey: request.requestKey,
      projectId,
      requestId: request.id,
      requesterId,
      durationSeconds: request.requestedDurationSeconds,
      providerBinding: AUTHORING_RESOURCE_PROVIDER_BINDING,
    })
  }
}

async function waitForStudentEnvironmentWithResourceApproval(request, adminPage, projectId, environmentId, requesterId) {
  let latest
  await expect.poll(async () => {
    const environmentResponse = await request.get(`/api/v1/environments/${environmentId}`)
    latest = await expectJson(environmentResponse, 'LAB_EXPERIMENT_STUDENT_ENVIRONMENT_READ_FAILED')
    if (latest.observedState === 'ready') return true
    if (['failed', 'deleted'].includes(latest.observedState)) {
      throw new Error(`LAB_EXPERIMENT_STUDENT_ENVIRONMENT_FAILED:${latest.observedState}`)
    }

    const requestsResponse = await adminPage.request.get(`/api/v1/projects/${projectId}/resource-requests`)
    const requests = await expectJson(requestsResponse, 'LAB_EXPERIMENT_STUDENT_RESOURCE_REQUESTS_READ_FAILED')
    if (!Array.isArray(requests)) throw new Error('LAB_EXPERIMENT_STUDENT_RESOURCE_REQUESTS_INVALID')
    const pending = requests.filter((item) => (
      item?.projectId === projectId
      && item?.requesterId === requesterId
      && item?.target?.kind === 'environment'
      && item?.target?.environmentId === environmentId
      && item.state === 'reviewing'
    ))
    for (const resourceRequest of pending) {
      if (!Number.isInteger(resourceRequest.requestedDurationSeconds) || resourceRequest.requestedDurationSeconds <= 0) {
        throw new Error(`LAB_EXPERIMENT_STUDENT_RESOURCE_DURATION_INVALID:${resourceRequest.id ?? 'missing'}`)
      }
      await approveResourceRequestByUi(adminPage, {
        requestKey: resourceRequest.requestKey,
        projectId,
        requestId: resourceRequest.id,
        environmentId,
        requesterId,
        durationSeconds: resourceRequest.requestedDurationSeconds,
        providerBinding: AUTHORING_RESOURCE_PROVIDER_BINDING,
      })
    }
    return false
  }, { timeout: 600_000, intervals: [1000, 2000, 3000] }).toBe(true)
  return latest
}

async function waitForBuiltCandidate(request, projectId, candidateId, onPendingResourceRequests = null) {
  const candidate = await pollEnvironmentCandidate(
    request,
    projectId,
    candidateId,
    async (value) => {
      if (!['succeeded', 'failed', 'cancelled'].includes(value.build?.state)) {
        await onPendingResourceRequests?.()
      }
      return ['succeeded', 'failed', 'cancelled'].includes(value.build?.state)
    },
    'LAB_EXPERIMENT_CANDIDATE_BUILD_STATUS_FAILED',
    CANDIDATE_BUILD_TIMEOUT_MS,
  )
  if (candidate.build?.state !== 'succeeded') {
    throw new Error(`LAB_EXPERIMENT_CANDIDATE_BUILD_FAILED:${candidate.build?.diagnosticCode ?? 'diagnostic missing'}`)
  }
  const artifact = candidate.imageArtifact
  if (!artifact || typeof artifact.repository !== 'string' || !/^sha256:[0-9a-f]{64}$/i.test(artifact.digest ?? '')) {
    throw new Error('LAB_EXPERIMENT_IMAGE_ARTIFACT_MISSING')
  }
  return { candidate, artifact }
}

async function approveAndPublish(page, projectId, runId, packageData, environmentCandidateId, evaluationCandidateId, artifact) {
  await page.goto(`/teacher/approvals?projectId=${encodeURIComponent(projectId)}&runId=${encodeURIComponent(runId)}`, {
    waitUntil: 'domcontentloaded',
  })
  await selectProjectByUi(page, projectId)
  await expect(page.getByRole('heading', { name: '实验包批准', exact: true })).toBeVisible()
  await expect(page.locator('input[aria-label="AgentRun ID"]')).toHaveValue(runId)
  const candidateCards = page.locator('.candidate-card')
  await expect(candidateCards).toHaveCount(2, { timeout: 120_000 })
  for (let index = 0; index < 2; index += 1) {
    await candidateCards.nth(index).locator('summary', { hasText: '查看候选技术详情' }).click()
  }
  await expect(page.getByText(environmentCandidateId, { exact: true }).first()).toBeVisible({ timeout: 120_000 })
  await expect(page.getByText(evaluationCandidateId, { exact: true }).first()).toBeVisible({ timeout: 120_000 })
  await expect(page.locator('.approval-card')).toBeVisible()

  const approvalButton = page.getByRole('button', { name: '批准完整实验包', exact: true })
  await expect(approvalButton).toBeDisabled()
  await page.getByRole('checkbox').check()
  await page.locator('textarea.reason-input').fill('已核对真实生成的 Environment、Evaluation 候选与构建产物摘要。')
  await expect(approvalButton).toBeEnabled({ timeout: 240_000 })

  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/projects/${projectId}/authoring-approvals`
  })
  await approvalButton.click()
  const approval = await expectJson(await responsePromise, 'LAB_EXPERIMENT_APPROVAL_FAILED')
  expect(approval.imageArtifact).toMatchObject({
    id: artifact.id,
    kind: 'container',
    repository: artifact.repository,
    digest: artifact.digest,
  })
  expect(approval.packageId).toBe(packageData.id)

  const publication = await pollJson(
    page.context().request,
    `/api/v1/projects/${projectId}/authoring-approvals/${approval.id}`,
    (value) => value.status === 'ready' || value.status === 'failed',
    'LAB_EXPERIMENT_PUBLICATION_STATUS_FAILED',
    600_000,
  )
  if (publication.status !== 'ready') {
    throw new Error(`LAB_EXPERIMENT_PUBLICATION_FAILED:${publication.diagnosticCode ?? 'diagnostic missing'}`)
  }
  return { approval, publication }
}

async function readExistingPublishedApproval(
  page,
  projectId,
  targetRunId,
  run,
  packageData,
  environmentCandidateId,
  evaluationCandidateId,
  artifact,
  approvalId,
) {
  const request = page.context().request
  const publication = await expectJson(
    await request.get(
      `/api/v1/projects/${encodeURIComponent(projectId)}/authoring-approvals/${encodeURIComponent(approvalId)}`,
    ),
    'LAB_EXPERIMENT_RESUME_APPROVAL_READ_FAILED',
  )
  if (publication === null || typeof publication !== 'object') {
    throw new Error('LAB_EXPERIMENT_RESUME_APPROVAL_RESPONSE_INVALID')
  }
  const approval = publication.approval
  const runEnvironmentCandidateId = run.tracks.find((track) => track.kind === 'environment')?.candidateId
  const runEvaluationCandidateId = run.tracks.find((track) => track.kind === 'evaluation')?.candidateId
  if (publication.status !== 'ready') {
    throw new Error(`LAB_EXPERIMENT_RESUME_APPROVAL_NOT_READY:${publication.status ?? 'missing'}`)
  }
  if (typeof publication.environmentReleaseId !== 'string' || !publication.environmentReleaseId) {
    throw new Error('LAB_EXPERIMENT_RESUME_APPROVAL_ENVIRONMENT_RELEASE_MISSING')
  }
  if (
    run.id !== targetRunId
    || run.projectId !== projectId
    || run.packageId !== packageData.id
    || runEnvironmentCandidateId !== environmentCandidateId
    || runEvaluationCandidateId !== evaluationCandidateId
    || approval?.id !== approvalId
    || approval.projectId !== projectId
    || approval.packageId !== packageData.id
    || approval.environmentCandidateId !== environmentCandidateId
    || approval.evaluationCandidateId !== evaluationCandidateId
  ) {
    throw new Error('LAB_EXPERIMENT_RESUME_APPROVAL_SCOPE_OR_PUBLICATION_INVALID')
  }
  // AuthoringApproval has no runId field; the exact candidate pair binds it to this AgentRun.
  expect(approval.imageArtifact).toEqual(artifact)

  const query = new URLSearchParams({ projectId, runId: targetRunId, approvalId })
  await page.goto(`/teacher/approvals?${query.toString()}`, { waitUntil: 'domcontentloaded' })
  await selectProjectByUi(page, projectId)
  await expect(page.getByRole('heading', { name: '实验包批准', exact: true })).toBeVisible()
  await expect(page.locator('input[aria-label="AgentRun ID"]')).toHaveValue(targetRunId)

  const approvalSuccess = page.locator('.approval-success')
  await expect(approvalSuccess).toBeVisible({ timeout: 120_000 })
  const approvalDetails = approvalSuccess.locator('details')
  await approvalDetails.locator('summary').click()
  await expect(approvalDetails.getByText(approvalId, { exact: true })).toBeVisible()

  const publicationCard = page.locator('.publication-status')
  await expect(publicationCard).toBeVisible({ timeout: 120_000 })
  await expect(publicationCard).toHaveAttribute('data-status', 'ready')
  await expect(publicationCard).toContainText('Environment 与 Evaluation 已发布，可以继续配置学生实验。')
  const publicationDetails = publicationCard.locator('details')
  await publicationDetails.locator('summary').click()
  await expect(publicationDetails.getByText(publication.environmentReleaseId, { exact: true })).toBeVisible()

  return { approval, publication }
}

async function createEnvironmentByStudentUi(page, projectId, releaseId) {
  if (page.url() === 'about:blank') await navigateFromHomeByUi(page, '我的实验')
  else await page.goto(`/student/labs?projectId=${encodeURIComponent(projectId)}`, { waitUntil: 'domcontentloaded' })
  await selectProjectByUi(page, projectId)
  await page.getByRole('button', { name: /创建项目环境/ }).first().click()
  const dialog = page.getByRole('dialog', { name: '创建项目环境', exact: true })
  await expect(dialog).toBeVisible()
  const releaseCard = dialog.locator('.release-card').filter({ hasText: releaseId }).first()
  await expect(releaseCard).toBeVisible({ timeout: 120_000 })
  const responsePromise = page.waitForResponse((response) => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/environments')
  await releaseCard.getByRole('button', { name: '选择并创建', exact: true }).click()
  const accepted = await expectJson(await responsePromise, 'LAB_EXPERIMENT_ENVIRONMENT_CREATE_FAILED')
  expect(accepted).toMatchObject({ environmentId: expect.any(String), operationId: expect.any(String) })
  return accepted.environmentId
}

async function freezeStudentSourceByUi(page, projectId, environmentId, frozenPath) {
  await page.goto(`/student/environments?projectId=${encodeURIComponent(projectId)}&environmentId=${encodeURIComponent(environmentId)}`, {
    waitUntil: 'domcontentloaded',
  })
  await page.getByRole('button', { name: '实验提交与凭据', exact: true }).click()
  const startButton = page.getByRole('button', { name: '发起冻结提交', exact: true })
  await expect(startButton).toBeEnabled({ timeout: 120_000 })
  await startButton.click()
  const dialog = page.getByRole('dialog', { name: '确认冻结清单', exact: true })
  await expect(dialog).toBeVisible()
  await expect(dialog.locator('.freeze-manifest')).toContainText(frozenPath)
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/environments/${environmentId}/freeze`
  })
  await dialog.getByRole('button', { name: '确认冻结', exact: true }).click()
  const accepted = await expectJson(await responsePromise, 'LAB_EXPERIMENT_FREEZE_ACCEPT_FAILED')
  const statusMatch = typeof accepted.statusUrl === 'string'
    ? accepted.statusUrl.match(/^\/api\/v1\/projects\/([^/?#]+)\/frozen-submissions\/([0-9a-f-]{36})$/)
    : null
  if (statusMatch?.[1] !== projectId || !statusMatch?.[2]) {
    throw new Error('LAB_EXPERIMENT_FREEZE_STATUS_URL_INVALID')
  }
  const frozen = await waitForFrozenSubmission(page.request, projectId, statusMatch[2], frozenPath)
  await expect(page.locator('.evidence-card')).toContainText(statusMatch[2], { timeout: 120_000 })
  return frozen
}

async function readCtfFlagByUi(page, connectUrl) {
  await page.goto(connectUrl, { waitUntil: 'domcontentloaded' })
  const connectPath = new URL(connectUrl, page.url()).pathname
  if (!connectPath.endsWith('/')) throw new Error('LAB_EXPERIMENT_CTF_CONNECT_URL_NOT_CANONICAL')
  await expect(page.getByRole('heading', { name: 'Flag Vault', exact: true })).toBeVisible({ timeout: 120_000 })
  const loginForm = page.locator('form').first()
  await expect(loginForm).toHaveAttribute('action', './login')
  const loginAction = await loginForm.getAttribute('action')
  if (new URL(loginAction, new URL(connectUrl, page.url())).pathname !== `${connectPath}login`) {
    throw new Error('LAB_EXPERIMENT_CTF_LOGIN_ACTION_SCOPE_INVALID')
  }
  await page.getByLabel('Username', { exact: true }).fill("' OR 1=1--")
  await page.getByLabel('Password', { exact: true }).fill('incorrect-password')
  await Promise.all([
    page.waitForNavigation({ waitUntil: 'domcontentloaded' }),
    page.getByRole('button', { name: 'Sign in', exact: true }).click(),
  ])
  const body = await page.locator('body').innerText()
  const flag = body.match(/FLAG\{[A-Za-z0-9_:-]{1,120}\}/)?.[0]
  if (!flag) throw new Error('LAB_EXPERIMENT_CTF_FLAG_NOT_DISCLOSED')
  return flag
}

async function submitCtfFlagByUi(page, connectUrl, flag) {
  await page.goto(connectUrl, { waitUntil: 'domcontentloaded' })
  const connectPath = new URL(connectUrl, page.url()).pathname
  if (!connectPath.endsWith('/')) throw new Error('LAB_EXPERIMENT_CTF_CONNECT_URL_NOT_CANONICAL')
  const submitForm = page.locator('form').nth(1)
  await expect(submitForm).toHaveAttribute('action', './submit')
  const submitAction = await submitForm.getAttribute('action')
  if (new URL(submitAction, new URL(connectUrl, page.url())).pathname !== `${connectPath}submit`) {
    throw new Error('LAB_EXPERIMENT_CTF_SUBMIT_ACTION_SCOPE_INVALID')
  }
  await page.getByLabel('Flag', { exact: true }).fill(flag)
  await Promise.all([
    page.waitForNavigation({ waitUntil: 'domcontentloaded' }),
    page.getByRole('button', { name: 'Submit proof', exact: true }).click(),
  ])
  await expect(page.locator('body')).toContainText(`proof recorded: ${flag}`)
}

async function revokeEnvironmentAccessGrants(page, projectId, environmentId) {
  const listResponse = await page.request.get(`/api/v1/environments/${environmentId}/access-grants?includeTerminal=false&limit=100`)
  if (listResponse.status() === 404) return 0
  const listed = await expectJson(listResponse, 'LAB_EXPERIMENT_ACCESS_GRANTS_CLEANUP_LIST_FAILED')
  let revokedCount = 0
  for (const item of listed.items ?? []) {
    let grantResponse = await page.request.get(`/api/v1/access-grants/${item.id}`)
    if (grantResponse.status() === 404) continue
    let grant = await expectJson(grantResponse, 'LAB_EXPERIMENT_ACCESS_GRANT_CLEANUP_READ_FAILED')
    if (!['requested', 'active'].includes(grant.state)) continue
    if (grant.state === 'requested') {
      throw new Error(`LAB_EXPERIMENT_ACCESS_GRANT_REQUESTED_NOT_VISIBLE:${grant.id}`)
    }
    await revokeEnvironmentAccessGrantByUi(page, projectId, environmentId, grant.id)
    grant = await pollJson(
      page.request,
      `/api/v1/access-grants/${grant.id}`,
      (value) => ['revoked', 'denied', 'expired'].includes(value.state),
      'LAB_EXPERIMENT_ACCESS_GRANT_CLEANUP_STATUS_FAILED',
      120_000,
    )
    if (grant.state !== 'revoked') throw new Error(`LAB_EXPERIMENT_ACCESS_GRANT_NOT_REVOKED:${grant.state}`)
    revokedCount += 1
  }
  return revokedCount
}

async function closeExperimentEnvironment(page, projectId, environmentId) {
  await revokeEnvironmentAccessGrants(page, projectId, environmentId)
  await deleteEnvironmentByUi(page, {
    routePrefix: 'student',
    projectId,
    environmentId,
    label: 'LAB_EXPERIMENT_ENVIRONMENT_DELETE',
  })
}

test('student completes a published lab experiment through its browser entry', async ({ browser, page, baseURL }, testInfo) => {
  test.setTimeout(FULL_CHAIN_TIMEOUT_MS)
  const request = page.context().request
  const teacherGuards = installUsabilityGuards(page)
  const resumeProjectId = process.env.LABWEAVER_E2E_LAB_RESUME_PROJECT_ID?.trim() ?? ''
  const resumeRunId = process.env.LABWEAVER_E2E_LAB_RESUME_RUN_ID?.trim() ?? ''
  if (Boolean(resumeProjectId) !== Boolean(resumeRunId)) {
    throw new Error('LAB_EXPERIMENT_RESUME_TARGET_INCOMPLETE')
  }
  const resumeExistingRun = Boolean(resumeProjectId && resumeRunId)
  if (RETAIN_CUDA_SAMPLE && resumeExistingRun) {
    throw new Error('LAB_EXPERIMENT_RETAIN_CUDA_SAMPLE_REQUIRES_NEW_PROJECT')
  }
  const resumeRetryTrack = process.env.LABWEAVER_E2E_LAB_RESUME_RETRY_TRACK?.trim() ?? ''
  if (resumeRetryTrack && !['environment', 'evaluation'].includes(resumeRetryTrack)) {
    throw new Error('LAB_EXPERIMENT_RESUME_RETRY_TRACK_INVALID')
  }
  if (resumeRetryTrack && !resumeExistingRun) {
    throw new Error('LAB_EXPERIMENT_RESUME_RETRY_TRACK_REQUIRES_TARGET')
  }
  const resumeApprovalId = process.env.LABWEAVER_E2E_LAB_RESUME_APPROVAL_ID?.trim() ?? ''
  if (resumeApprovalId && !resumeExistingRun) {
    throw new Error('LAB_EXPERIMENT_RESUME_APPROVAL_REQUIRES_TARGET')
  }
  if (resumeRetryTrack && resumeApprovalId) {
    throw new Error('LAB_EXPERIMENT_RESUME_RETRY_TRACK_CONFLICTS_WITH_APPROVAL')
  }
  const packageCopy = resumeExistingRun ? null : await mkdtemp(join(tmpdir(), 'labweaver-lab-'))
  let projectId = resumeProjectId || null
  let environmentId
  let studentContext
  let studentPage
  let adminContext
  let primaryFailure
  let primaryFailed = false
  let environmentReleased = false
  let retainedCudaSampleEnvironmentId = null
  let retainedCudaSampleCompleted = false
  let baselineChargeIds = new Set()
  const cleanupErrors = []
  const frames = []
  const terminalSockets = new Map()
  const captureTerminalSocket = (socket) => {
    const receive = ({ payload }) => {
      frames.push(typeof payload === 'string' ? payload : Buffer.from(payload).toString('utf8'))
    }
    terminalSockets.set(socket, receive)
    socket.on('framereceived', receive)
  }
  try {
    let project
    let packageData
    let run
    let completed
    const teacherActorId = await readActorId(request)
    adminContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
    const adminPage = await adminContext.newPage()
    if (resumeExistingRun) {
      project = { id: resumeProjectId }
      const existing = await expectJson(
        await request.get(`/api/v1/projects/${encodeURIComponent(project.id)}/agent-runs/${encodeURIComponent(resumeRunId)}`),
        'LAB_EXPERIMENT_RESUME_RUN_READ_FAILED',
      )
      if (
        existing.id !== resumeRunId
        || existing.projectId !== project.id
        || existing.purpose?.kind !== 'authoring'
        || existing.purpose.environmentClass !== 'experiment'
        || typeof existing.packageId !== 'string'
      ) {
        throw new Error('LAB_EXPERIMENT_RESUME_RUN_SCOPE_INVALID')
      }
      run = existing
      packageData = { id: existing.packageId }
      if (resumeApprovalId) {
        if (!runIsFullyTerminal(run) || run.state !== 'succeeded') {
          throw new Error('LAB_EXPERIMENT_RESUME_APPROVAL_REQUIRES_SUCCESSFUL_RUN')
        }
        completed = await waitForExperimentRun(request, project.id, run.id)
      } else {
        if (!runIsFullyTerminal(run)) {
          run = await waitForTerminalExperimentRun(
            request,
            project.id,
            run.id,
            () => approveAuthoringResourceRequestsByUi(adminPage, project.id, run.id, teacherActorId),
          )
        }
        let retryWaitOptions = {}
        const automaticEnvironmentRetry = !resumeRetryTrack && isFirstFailedTrackAttempt(run, 'environment')
        const requestedRetryTrack = resumeRetryTrack || (automaticEnvironmentRetry ? 'environment' : '')
        if (requestedRetryTrack) {
          const acceptedRetry = await retryFailedTrackByUi(page, project.id, run, requestedRetryTrack)
          retryWaitOptions = {
            minimumRevision: acceptedRetry.revision,
            requiredTrackKind: requestedRetryTrack,
            requiredTrackAttemptNumber: 2,
          }
        } else if (runHasManualRetry(run)) {
          throw new Error('LAB_EXPERIMENT_RESUME_ENVIRONMENT_RETRY_ALREADY_USED')
        }
        completed = await waitForExperimentRun(
          request,
          project.id,
          run.id,
          () => approveAuthoringResourceRequestsByUi(adminPage, project.id, run.id, teacherActorId),
          retryWaitOptions,
        )
      }
    } else {
      await cp(LAB.root, packageCopy, { recursive: true })
      await configureLabPackageCopy(packageCopy)
      if (process.env.LABWEAVER_E2E_LAB === 'xv6') {
        const sourcePath = join(packageCopy, LAB.frozenPath)
        const source = await readFile(sourcePath, 'utf8')
        const starterSource = source.replace('xv6-student: hello', LAB.starterOutput)
        if (starterSource === source) throw new Error('LAB_EXPERIMENT_XV6_STARTER_OUTPUT_NOT_FOUND')
        await writeFile(sourcePath, starterSource, 'utf8')

        const manifestPath = join(packageCopy, 'manifest.json')
        const manifest = JSON.parse(await readFile(manifestPath, 'utf8'))
        const manifestFile = manifest.spec?.files?.find((file) => file.path === LAB.frozenPath)
        if (!manifestFile) throw new Error('LAB_EXPERIMENT_XV6_MANIFEST_ENTRY_MISSING')
        manifestFile.sha256 = createHash('sha256').update(starterSource).digest('hex')
        await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8')
      }
      project = await createProjectByUi(page, `real-${process.env.LABWEAVER_E2E_LAB}-${Date.now()}-${uuidv7().slice(0, 8)}`)
      projectId = project.id
      await selectProjectByUi(page, project.id)
      await configureProjectPolicyByUi(page, project.id)
      await page.goto(`/teacher/materials?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
      await selectProjectByUi(page, project.id)
      if (RETAIN_CUDA_SAMPLE) await selectPermanentMaterialRetentionByUi(page)
      packageData = await uploadPackageDirectoryByUi(page, packageCopy, LAB.uploadCheckPath ?? LAB.frozenPath)
      if (RETAIN_CUDA_SAMPLE) {
        expect(packageData.retention).toMatchObject({
          retainUntil: null,
          disposition: 'retain_until_revoked',
        })
      }
      run = await startExperimentRunByUi(page, project.id)
      completed = await waitForExperimentRun(
        request,
        project.id,
        run.id,
        () => approveAuthoringResourceRequestsByUi(adminPage, project.id, run.id, teacherActorId),
      )
    }
    const built = await waitForBuiltCandidate(
      request,
      project.id,
      completed.environmentCandidateId,
      resumeApprovalId
        ? null
        : () => approveAuthoringResourceRequestsByUi(adminPage, project.id, completed.run.id, teacherActorId),
    )
    const published = resumeApprovalId
      ? await readExistingPublishedApproval(
        page,
        project.id,
        resumeRunId,
        completed.run,
        packageData,
        completed.environmentCandidateId,
        completed.evaluationCandidateId,
        built.artifact,
        resumeApprovalId,
      )
      : await approveAndPublish(
        page,
        project.id,
        completed.run.id,
        packageData,
        completed.environmentCandidateId,
        completed.evaluationCandidateId,
        built.artifact,
      )
    if (RETAIN_CUDA_SAMPLE) {
      const publishedCandidate = await expectJson(
        await request.get(
          `/api/v1/projects/${encodeURIComponent(project.id)}/environment-candidates/${encodeURIComponent(completed.environmentCandidateId)}`,
        ),
        'LAB_EXPERIMENT_CUDA_PERMANENT_CANDIDATE_READ_FAILED',
      )
      expect(publishedCandidate).toMatchObject({
        candidate: {
          id: completed.environmentCandidateId,
          projectId: project.id,
          spec: {
            retention: {
              retainUntil: null,
              disposition: 'retain_until_revoked',
            },
          },
        },
      })
      const publishedRelease = await expectJson(
        await request.get(
          `/api/v1/projects/${encodeURIComponent(project.id)}/environment-template-releases/${encodeURIComponent(published.publication.environmentReleaseId)}`,
        ),
        'LAB_EXPERIMENT_CUDA_PERMANENT_RELEASE_READ_FAILED',
      )
      expect(publishedRelease).toMatchObject({
        id: published.publication.environmentReleaseId,
        projectId: project.id,
        candidateId: completed.environmentCandidateId,
        approval: { decision: 'approved' },
      })
    }
    await assertNoStuckProgress(page, 'teacher-approval')
    await auditAccessibility(page, 'teacher-approval', testInfo)
    teacherGuards.assertCleanConsole('teacher-approval')

    studentContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.student })
    studentPage = await studentContext.newPage()
    const studentGuards = installUsabilityGuards(studentPage)
    const studentActorId = await readActorId(studentContext.request)
    await addProjectStudentByUi(page, project.id)
    const baselineCharges = await expectJson(
      await adminPage.request.get(`/api/v1/projects/${encodeURIComponent(project.id)}/charges`),
      'LAB_EXPERIMENT_BASELINE_CHARGES_READ_FAILED',
    )
    if (!Array.isArray(baselineCharges)) throw new Error('LAB_EXPERIMENT_BASELINE_CHARGES_INVALID')
    baselineChargeIds = new Set(baselineCharges
      .map((charge) => charge.id)
      .filter((id) => typeof id === 'string' && id !== ''))
    environmentId = await createEnvironmentByStudentUi(studentPage, project.id, published.publication.environmentReleaseId)
    const environment = await waitForStudentEnvironmentWithResourceApproval(
      studentContext.request,
      adminPage,
      project.id,
      environmentId,
      studentActorId,
    )
    if (RETAIN_CUDA_SAMPLE) expect(environment.eligibilityExpiresAt).toBeNull()
    if (LAB.gpuMode) {
      expect(environment.gpuAllocation?.mode, 'LAB_EXPERIMENT_GPU_MODE_MISSING').toBe(LAB.gpuMode)
      expect(environment.gpuAllocation?.class, 'LAB_EXPERIMENT_GPU_CLASS_MISSING').toBe(LAB.gpuClass)
    }
    await assertNoStuckProgress(studentPage, 'student-environment')
    await auditAccessibility(studentPage, 'student-environment', testInfo)
    const beforeRequestIds = await snapshotProjectResourceRequestIds(adminPage.request, project.id)

    const isCtfLab = LAB.kind === 'ctf'
    let terminal = null
    let httpGrant = null
    let ctfFlag = null
    if (isCtfLab) {
      const issued = await issueEnvironmentAccessGrantByUi(studentPage, project.id, environment, 'http')
      httpGrant = issued.endpointGrant
      expect(httpGrant.connectUrl).toBe(`/connect/${httpGrant.id}/`)
      ctfFlag = await readCtfFlagByUi(studentPage, httpGrant.connectUrl)
      await submitCtfFlagByUi(studentPage, httpGrant.connectUrl, 'FLAG{student-first-attempt}')
    } else {
      studentPage.on('websocket', captureTerminalSocket)
      terminal = await issueAccessGrantAndConnect(studentPage, project.id, environmentId)
    }
    if (process.env.LABWEAVER_E2E_LAB === 'cuda') {
      const starterCommand = "grep -Fx '#define BLOCKS 2' student/gpu_stats.cu && /usr/local/cuda/bin/nvcc -o gpu_stats student/gpu_stats.cu && ./gpu_stats > student/result.txt && cat student/result.txt"
      const output = await typeTerminalCommand(studentPage, terminal.input, frames, starterCommand, 'LABWEAVER_LAB_STARTER_DONE')
      expect(hasTerminalLine(output, 'N=256'), 'LAB_EXPERIMENT_GPU_STARTER_COUNT_MISSING').toBe(true)
      expect(hasTerminalLine(output, 'sum=2016'), 'LAB_EXPERIMENT_GPU_STARTER_SUM_MISSING').toBe(true)
      expect(hasTerminalLine(output, 'max=63'), 'LAB_EXPERIMENT_GPU_STARTER_MAX_MISSING').toBe(true)
    }
    const frozen = await freezeStudentSourceByUi(studentPage, project.id, environmentId, LAB.frozenPath)
    const firstResult = await waitForProjectEvaluationResultWithResourceApproval({
      request: studentContext.request,
      adminPage,
      projectId: project.id,
      frozenSubmissionId: frozen.id,
      studentActorId,
      existingRequestIds: beforeRequestIds,
    })
    if (LAB.expectImprovement) {
      expect(firstResult.awardedScore).toBeLessThan(firstResult.maxScore)
    } else {
      expect(firstResult.awardedScore).toBe(firstResult.maxScore)
    }

    if (isCtfLab) {
      if (!ctfFlag || !httpGrant) throw new Error('LAB_EXPERIMENT_CTF_CONTEXT_MISSING')
      expect(firstResult.awardedScore).toBeLessThan(firstResult.maxScore)
      const afterRequestIds = await snapshotProjectResourceRequestIds(adminPage.request, project.id)
      await submitCtfFlagByUi(studentPage, httpGrant.connectUrl, ctfFlag)
      const afterFrozen = await freezeStudentSourceByUi(studentPage, project.id, environmentId, LAB.frozenPath)
      const afterResult = await waitForProjectEvaluationResultWithResourceApproval({
        request: studentContext.request,
        adminPage,
        projectId: project.id,
        frozenSubmissionId: afterFrozen.id,
        studentActorId,
        existingRequestIds: afterRequestIds,
      })
      expect(afterResult.awardedScore).toBeGreaterThan(firstResult.awardedScore)
      expect(afterResult.awardedScore).toBe(afterResult.maxScore)
      await studentPage.goto(`/student/results?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
      const card = studentPage.locator('.result-card').filter({ hasText: afterResult.runId })
      await expect(card).toHaveCount(1, { timeout: 120_000 })
      await expect(card.locator('.result-score')).toHaveText(`${afterResult.awardedScore} / ${afterResult.maxScore}`)
    } else if (LAB.fixCommands.length > 0) {
      terminal = await issueAccessGrantAndConnect(studentPage, project.id, environmentId)
      for (const [index, command] of LAB.fixCommands.entries()) {
        const output = await typeTerminalCommand(studentPage, terminal.input, frames, command, `LABWEAVER_LAB_DONE_${index}`)
        if (LAB.gpuMode && index === LAB.fixCommands.length - 1) {
          expect(hasTerminalLine(output, 'N=256'), 'LAB_EXPERIMENT_GPU_COUNT_MISSING').toBe(true)
          expect(hasTerminalLine(output, 'sum=32640'), 'LAB_EXPERIMENT_GPU_SUM_MISSING').toBe(true)
          expect(hasTerminalLine(output, 'max=255'), 'LAB_EXPERIMENT_GPU_MAX_MISSING').toBe(true)
        }
      }
      const afterRequestIds = await snapshotProjectResourceRequestIds(adminPage.request, project.id)
      const afterFrozen = await freezeStudentSourceByUi(studentPage, project.id, environmentId, LAB.frozenPath)
      const afterResult = await waitForProjectEvaluationResultWithResourceApproval({
        request: studentContext.request,
        adminPage,
        projectId: project.id,
        frozenSubmissionId: afterFrozen.id,
        studentActorId,
        existingRequestIds: afterRequestIds,
      })
      expect(afterResult.awardedScore).toBeGreaterThan(firstResult.awardedScore)
      expect(afterResult.awardedScore).toBe(afterResult.maxScore)
        await studentPage.goto(`/student/results?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
      const card = studentPage.locator('.result-card').filter({ hasText: afterResult.runId })
      await expect(card).toHaveCount(1, { timeout: 120_000 })
      await expect(card.locator('.result-score')).toHaveText(`${afterResult.awardedScore} / ${afterResult.maxScore}`)
    } else {
      await studentPage.goto(`/student/results?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
      const card = studentPage.locator('.result-card').filter({ hasText: firstResult.runId })
      await expect(card).toHaveCount(1, { timeout: 120_000 })
      await expect(card.locator('.result-score')).toHaveText(`${firstResult.awardedScore} / ${firstResult.maxScore}`)
    }
    await assertNoStuckProgress(studentPage, 'student-results')
    await auditAccessibility(studentPage, 'student-results', testInfo)
    studentGuards.assertCleanConsole('student-results')

    await closeExperimentEnvironment(studentPage, project.id, environmentId)
    const financeRates = await readResourceRates(adminPage, 'LAB_EXPERIMENT_RATES_READ_FAILED')
    const finance = await waitForSettledExperimentUsageCharges(browser, baseURL, {
      projectId: project.id,
      environmentId,
      baselineChargeIds,
      gpu: LAB.gpuMode
        ? { class: environment.gpuAllocation.class, mode: environment.gpuAllocation.mode }
        : null,
      rates: financeRates,
    })
    await inspectRealWorkFinanceByUi(browser, baseURL, project.id, {
      gpu: LAB.gpuMode
        ? { class: environment.gpuAllocation.class, mode: environment.gpuAllocation.mode }
        : null,
      requireBudget: false,
      usageRecordIds: finance.matches.map(({ usage }) => usage.id),
      expectedCharges: finance.matches.map(({ charge }) => charge),
    })
    environmentReleased = true

    if (RETAIN_CUDA_SAMPLE) {
      retainedCudaSampleEnvironmentId = await createEnvironmentByStudentUi(
        studentPage,
        project.id,
        published.publication.environmentReleaseId,
      )
      const retainedEnvironment = await waitForStudentEnvironmentWithResourceApproval(
        studentContext.request,
        adminPage,
        project.id,
        retainedCudaSampleEnvironmentId,
        studentActorId,
      )
      expect(retainedEnvironment).toMatchObject({
        id: retainedCudaSampleEnvironmentId,
        observedState: 'ready',
        desiredState: 'running',
        eligibilityExpiresAt: null,
        gpuAllocation: {
          mode: 'exclusive',
          class: LAB.gpuClass,
        },
      })

      let sampleTerminal = await issueAccessGrantAndConnect(
        studentPage,
        project.id,
        retainedCudaSampleEnvironmentId,
      )
      const sampleStarterCommand = "grep -Fx '#define BLOCKS 2' student/gpu_stats.cu && /usr/local/cuda/bin/nvcc -o gpu_stats student/gpu_stats.cu && ./gpu_stats > student/result.txt && cat student/result.txt"
      const sampleStarterOutput = await typeTerminalCommand(
        studentPage,
        sampleTerminal.input,
        frames,
        sampleStarterCommand,
        'LABWEAVER_CUDA_SAMPLE_STARTER_DONE',
      )
      expect(hasTerminalLine(sampleStarterOutput, 'N=256'), 'LAB_EXPERIMENT_CUDA_SAMPLE_STARTER_COUNT_MISSING').toBe(true)
      expect(hasTerminalLine(sampleStarterOutput, 'sum=2016'), 'LAB_EXPERIMENT_CUDA_SAMPLE_STARTER_SUM_MISSING').toBe(true)
      expect(hasTerminalLine(sampleStarterOutput, 'max=63'), 'LAB_EXPERIMENT_CUDA_SAMPLE_STARTER_MAX_MISSING').toBe(true)

      sampleTerminal = await issueAccessGrantAndConnect(
        studentPage,
        project.id,
        retainedCudaSampleEnvironmentId,
      )
      for (const [index, command] of LAB.fixCommands.entries()) {
        const output = await typeTerminalCommand(
          studentPage,
          sampleTerminal.input,
          frames,
          command,
          `LABWEAVER_CUDA_SAMPLE_DONE_${index}`,
        )
        if (index === LAB.fixCommands.length - 1) {
          expect(hasTerminalLine(output, 'N=256'), 'LAB_EXPERIMENT_CUDA_SAMPLE_COUNT_MISSING').toBe(true)
          expect(hasTerminalLine(output, 'sum=32640'), 'LAB_EXPERIMENT_CUDA_SAMPLE_SUM_MISSING').toBe(true)
          expect(hasTerminalLine(output, 'max=255'), 'LAB_EXPERIMENT_CUDA_SAMPLE_MAX_MISSING').toBe(true)
        }
      }

      const sampleBeforeRequestIds = await snapshotProjectResourceRequestIds(adminPage.request, project.id)
      const sampleFrozen = await freezeStudentSourceByUi(
        studentPage,
        project.id,
        retainedCudaSampleEnvironmentId,
        LAB.frozenPath,
      )
      const sampleResult = await waitForProjectEvaluationResultWithResourceApproval({
        request: studentContext.request,
        adminPage,
        projectId: project.id,
        frozenSubmissionId: sampleFrozen.id,
        studentActorId,
        existingRequestIds: sampleBeforeRequestIds,
      })
      expect(sampleResult.awardedScore).toBe(sampleResult.maxScore)
      await studentPage.goto(`/student/results?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
      const sampleCard = studentPage.locator('.result-card').filter({ hasText: sampleResult.runId })
      await expect(sampleCard).toHaveCount(1, { timeout: 120_000 })
      await expect(sampleCard.locator('.result-score')).toHaveText(`${sampleResult.awardedScore} / ${sampleResult.maxScore}`)

      const revokedGrantCount = await revokeEnvironmentAccessGrants(
        studentPage,
        project.id,
        retainedCudaSampleEnvironmentId,
      )
      if (revokedGrantCount < 1) throw new Error('LAB_EXPERIMENT_CUDA_SAMPLE_ACCESS_GRANT_NOT_REVOKED')

      const stoppedSample = await stopEnvironmentByUi(studentPage, {
        routePrefix: 'student',
        projectId: project.id,
        environmentId: retainedCudaSampleEnvironmentId,
        label: 'LAB_EXPERIMENT_CUDA_SAMPLE_STOP',
      })
      expect(stoppedSample).toMatchObject({
        id: retainedCudaSampleEnvironmentId,
        observedState: 'stopped',
        desiredState: 'stopped',
      })
      await studentPage.goto(
        `/student/environments?projectId=${encodeURIComponent(project.id)}&environmentId=${encodeURIComponent(retainedCudaSampleEnvironmentId)}`,
        { waitUntil: 'domcontentloaded' },
      )
      await expect(studentPage.getByRole('heading', { name: '项目环境控制台', exact: true })).toBeVisible({ timeout: 120_000 })
      const stoppedSampleRead = await expectJson(
        await studentPage.request.get(`/api/v1/environments/${encodeURIComponent(retainedCudaSampleEnvironmentId)}`),
        'LAB_EXPERIMENT_CUDA_SAMPLE_STOPPED_READ_FAILED',
      )
      expect(stoppedSampleRead).toMatchObject({
        id: retainedCudaSampleEnvironmentId,
        observedState: 'stopped',
        desiredState: 'stopped',
        eligibilityExpiresAt: null,
      })
      await expect(studentPage.getByRole('button', { name: '启动', exact: true })).toBeEnabled({ timeout: 30_000 })
      await expect(studentPage.locator('#lifecycle-action-hint')).toContainText('工作目录和磁盘仍保留', { timeout: 120_000 })

      retainedCudaSampleCompleted = true
      console.log(`[LABWEAVER_CUDA_SAMPLE_READY] name=${project.name} project=${project.id} environment=${retainedCudaSampleEnvironmentId} release=${published.publication.environmentReleaseId}`)
    }
  } catch (error) {
    primaryFailure = error
    primaryFailed = true
  } finally {
    try {
      studentPage?.off('websocket', captureTerminalSocket)
      for (const [socket, receive] of terminalSockets) socket.off('framereceived', receive)
    } catch (error) {
      cleanupErrors.push(error)
    } finally {
      frames.length = 0
      terminalSockets.clear()
    }
    try {
      if (environmentId && !environmentReleased) {
        if (!studentContext) studentContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.student })
        const cleanupPage = await studentContext.newPage()
        if (!projectId) cleanupErrors.push(new Error('LAB_EXPERIMENT_CLEANUP_PROJECT_ID_MISSING'))
        else await closeExperimentEnvironment(cleanupPage, projectId, environmentId)
      }
    } catch (error) {
      cleanupErrors.push(error)
    }
    try {
      if (retainedCudaSampleEnvironmentId && !retainedCudaSampleCompleted) {
        if (!studentContext) studentContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.student })
        const cleanupPage = await studentContext.newPage()
        if (!projectId) cleanupErrors.push(new Error('LAB_EXPERIMENT_CUDA_SAMPLE_CLEANUP_PROJECT_ID_MISSING'))
        else await closeExperimentEnvironment(cleanupPage, projectId, retainedCudaSampleEnvironmentId)
      }
    } catch (error) {
      cleanupErrors.push(error)
    }
    for (const context of [studentContext, adminContext]) {
      try {
        await context?.close()
      } catch (error) {
        cleanupErrors.push(error)
      }
    }
    try {
      if (packageCopy) await rm(packageCopy, { recursive: true, force: true })
    } catch (error) {
      cleanupErrors.push(error)
    }
  }
  if (primaryFailed && cleanupErrors.length > 0) {
    throw new AggregateError([primaryFailure, ...cleanupErrors], 'LAB_EXPERIMENT_PRIMARY_AND_CLEANUP_FAILED')
  }
  if (primaryFailed) throw primaryFailure
  if (cleanupErrors.length > 0) throw new AggregateError(cleanupErrors, 'LAB_EXPERIMENT_CLEANUP_FAILED')
})
