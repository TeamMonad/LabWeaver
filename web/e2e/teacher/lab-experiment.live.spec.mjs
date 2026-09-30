import { expect, test } from '@playwright/test'
import {
  AUTH_STATE,
  createProjectByUi,
  configureProjectPolicyByUi,
  csrfHeaders,
  expectJson,
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
})

const LAB = LABS[process.env.LABWEAVER_E2E_LAB ?? '']
const AGENT_RUN_TIMEOUT_MS = Number(process.env.LABWEAVER_E2E_AGENT_RUN_TIMEOUT_MS) || 1_800_000
const AUTHORING_RESOURCE_PROVIDER_BINDING =
  process.env.LABWEAVER_E2E_AUTHORING_PROVIDER_BINDING?.trim()
  || process.env.LABWEAVER_E2E_PROVIDER_BINDING?.trim()
  || 'container-primary-v1'

test.skip(!LAB, 'Set LABWEAVER_E2E_LAB=xv6 or LABWEAVER_E2E_LAB=cuda for a real lab acceptance run.')

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
  const updatedEnvironment = lines.join('\n')
  await writeFile(environmentPath, updatedEnvironment, 'utf8')

  const manifestPath = join(packageCopy, 'manifest.json')
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'))
  const manifestFile = manifest.spec?.files?.find((file) => file.path === 'environment.yaml')
  if (!manifestFile) throw new Error('LAB_EXPERIMENT_GPU_MANIFEST_ENTRY_MISSING')
  manifestFile.sha256 = createHash('sha256').update(updatedEnvironment).digest('hex')
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8')
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
  { minimumRevision = 0, requiredEnvironmentAttemptNumber = null } = {},
) {
  return await pollJson(
    request,
    `/api/v1/projects/${projectId}/agent-runs/${runId}`,
    async (value) => {
      const revisionIsFresh = Number.isInteger(value.revision) && value.revision >= minimumRevision
      const requiredAttempt = requiredEnvironmentAttemptNumber === null
        ? null
        : value.tracks.find((track) => track.kind === 'environment')?.attempts
          ?.find((attempt) => attempt.number === requiredEnvironmentAttemptNumber)
      const requiredAttemptIsTerminal = requiredEnvironmentAttemptNumber === null
        || ['succeeded', 'failed', 'cancelled'].includes(requiredAttempt?.state)
      const complete = runIsFullyTerminal(value) && revisionIsFresh && requiredAttemptIsTerminal
      if (!complete) await onPendingResourceRequests?.()
      return complete
    },
    'LAB_EXPERIMENT_AGENT_RUN_STATUS_FAILED',
    AGENT_RUN_TIMEOUT_MS,
  )
}

function isFirstFailedEnvironmentAttempt(run) {
  if (run.state !== 'failed' && run.state !== 'partially_succeeded') return false
  const track = run.tracks.find((item) => item.kind === 'environment')
  const attempts = track?.attempts ?? []
  const last = attempts[attempts.length - 1]
  return attempts.length === 1
    && last?.number === 1
    && (last.state === 'failed' || last.state === 'cancelled')
}

function environmentRetryAlreadyUsed(run) {
  if (run.state !== 'failed' && run.state !== 'partially_succeeded') return false
  const track = run.tracks.find((item) => item.kind === 'environment')
  const attempts = track?.attempts ?? []
  const last = attempts[attempts.length - 1]
  return (last?.state === 'failed' || last?.state === 'cancelled')
    && (attempts.length > 1 || last.number > 1)
}

async function retryFailedEnvironmentTrackByUi(page, projectId, run) {
  await page.goto(
    `/teacher/materials?projectId=${encodeURIComponent(projectId)}&packageId=${encodeURIComponent(run.packageId)}&runId=${encodeURIComponent(run.id)}`,
    { waitUntil: 'domcontentloaded' },
  )
  await selectProjectByUi(page, projectId)
  const retry = page.getByRole('button', { name: '重试环境轨道', exact: true })
  await expect(retry).toBeVisible({ timeout: 120_000 })
  await expect(retry).toBeEnabled()

  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/projects/${projectId}/agent-runs/${run.id}/tracks/environment/retry`
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
    600_000,
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

async function createEnvironmentByStudentUi(page, projectId, releaseId) {
  await page.goto(`/student/labs?projectId=${encodeURIComponent(projectId)}`, { waitUntil: 'domcontentloaded' })
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

async function issueAccessGrantAndConnect(page, projectId, environmentId) {
  await page.goto(`/student/environments?projectId=${encodeURIComponent(projectId)}&environmentId=${encodeURIComponent(environmentId)}`, {
    waitUntil: 'domcontentloaded',
  })
  const environmentIdDetails = page.locator('details.environment-id-details')
  await expect(environmentIdDetails).toBeVisible({ timeout: 120_000 })
  await environmentIdDetails.locator('summary').click()
  await expect(environmentIdDetails.locator('code')).toHaveText(environmentId, { timeout: 30_000 })
  const grantButton = page.getByRole('button', { name: '签发访问授权', exact: true })
  if (await grantButton.count() > 0) {
    await expect(grantButton).toBeEnabled({ timeout: 120_000 })
    await grantButton.click()
  }
  await pollJson(
    page.request,
    `/api/v1/environments/${environmentId}/access-grants?includeTerminal=false&limit=10`,
    (value) => Array.isArray(value.items) && value.items.some((item) => item.state === 'active'),
    'LAB_EXPERIMENT_ACCESS_GRANT_ACTIVE_TIMEOUT',
    120_000,
  )
  await page.getByRole('button', { name: 'Web 控制台', exact: true }).click()
  const reconnect = page.getByRole('button', { name: /重新连接终端|重新签发授权并连接终端|立即签发授权并连接终端/ })
  if (await reconnect.count() > 0) {
    await expect(reconnect).toBeEnabled({ timeout: 120_000 })
    await reconnect.click()
  }
  const consolePanel = page.locator('.console-panel')
  await expect(consolePanel).toBeVisible({ timeout: 120_000 })
  const openTerminal = consolePanel.getByRole('button', { name: '打开终端', exact: true })
  if (await openTerminal.count() > 0) {
    await expect(openTerminal).toBeEnabled({ timeout: 120_000 })
    await openTerminal.click()
  }
  const host = page.locator('.xterm-host')
  await expect(host).toBeVisible({ timeout: 120_000 })
  const input = page.locator('.xterm-helper-textarea')
  await expect(input).toBeAttached({ timeout: 30_000 })
  return { input }
}

function normalizeTerminalOutput(value) {
  const escape = String.fromCharCode(27)
  const bell = String.fromCharCode(7)
  const ansiPattern = new RegExp(`${escape}(?:\\[[0-?]*[ -/]*[@-~]|\\][^${bell}]*(?:${bell}|${escape}\\\\))`, 'g')
  return value
    .replace(ansiPattern, '')
    .replace(/\r/g, '')
}

function hasTerminalLine(output, expected) {
  const escaped = expected.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  return new RegExp(`(?:^|\\n)${escaped}(?:\\n|$)`).test(normalizeTerminalOutput(output))
}

async function typeTerminalCommand(page, input, frames, command, marker) {
  const firstFrame = frames.length
  const uniqueMarker = `${marker}-${uuidv7()}`
  await page.getByRole('button', { name: 'Web 控制台', exact: true }).click()
  const host = page.locator('.xterm-host')
  await expect(host).toBeVisible({ timeout: 120_000 })
  await host.click()
  await expect(input).toBeAttached({ timeout: 30_000 })
  await input.focus()
  await page.keyboard.type(`{ ${command}; __lw_exit=$?; printf '\\n${uniqueMarker}:%s\\n' "$__lw_exit"; }`)
  await page.keyboard.press('Enter')
  await expect
    .poll(() => {
      const output = normalizeTerminalOutput(frames.slice(firstFrame).join(''))
      const match = output.match(new RegExp(`(?:^|\\n)${uniqueMarker}:(\\d+)(?:\\n|$)`))
      return match?.[1] ?? null
    }, { timeout: 180_000, intervals: [250, 500, 1000] })
    .toBe('0')
  return normalizeTerminalOutput(frames.slice(firstFrame).join(''))
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

async function waitForCleanupEnvironmentState(request, environmentId) {
  let latest
  await expect.poll(async () => {
    const response = await request.get(`/api/v1/environments/${environmentId}`)
    if (response.status() === 404) {
      latest = { observedState: 'deleted' }
      return true
    }
    latest = await expectJson(response, 'LAB_EXPERIMENT_ENVIRONMENT_CLEANUP_READ_FAILED')
    return ['stopped', 'failed', 'deleting', 'deleted'].includes(latest.observedState)
  }, { timeout: 240_000, intervals: [1000, 2000, 3000] }).toBe(true)
  return latest
}

async function revokeEnvironmentAccessGrants(request, baseURL, environmentId) {
  const listResponse = await request.get(`/api/v1/environments/${environmentId}/access-grants?includeTerminal=false&limit=100`)
  if (listResponse.status() === 404) return
  const listed = await expectJson(listResponse, 'LAB_EXPERIMENT_ACCESS_GRANTS_CLEANUP_LIST_FAILED')
  for (const item of listed.items ?? []) {
    let grantResponse = await request.get(`/api/v1/access-grants/${item.id}`)
    if (grantResponse.status() === 404) continue
    let grant = await expectJson(grantResponse, 'LAB_EXPERIMENT_ACCESS_GRANT_CLEANUP_READ_FAILED')
    if (!['requested', 'active'].includes(grant.state)) continue
    const revokeResponse = await request.post(`/api/v1/access-grants/${grant.id}/revoke`, {
      headers: await csrfHeaders(request, baseURL, {
        'Idempotency-Key': uuidv7(),
        'If-Match': `"rev-${grant.revision}"`,
      }),
      data: { grantId: grant.id, reasonCode: 'lab_acceptance_cleanup' },
    })
    const revoked = await expectJson(revokeResponse, 'LAB_EXPERIMENT_ACCESS_GRANT_CLEANUP_REVOKE_FAILED')
    expect(revoked).toMatchObject({ id: grant.id, state: 'revoked' })
    grant = await pollJson(
      request,
      `/api/v1/access-grants/${grant.id}`,
      (value) => ['revoked', 'denied', 'expired'].includes(value.state),
      'LAB_EXPERIMENT_ACCESS_GRANT_CLEANUP_STATUS_FAILED',
      120_000,
    )
    if (grant.state !== 'revoked') throw new Error(`LAB_EXPERIMENT_ACCESS_GRANT_NOT_REVOKED:${grant.state}`)
  }
}

async function closeExperimentEnvironment(page, environmentId, baseURL) {
  const request = page.request
  await revokeEnvironmentAccessGrants(request, baseURL, environmentId)
  const currentResponse = await request.get(`/api/v1/environments/${environmentId}`)
  if (currentResponse.status() === 404) return
  let current = await expectJson(currentResponse, 'LAB_EXPERIMENT_ENVIRONMENT_READ_FOR_CLEANUP_FAILED')
  if (current.observedState === 'deleted') return
  if (current.observedState === 'ready') {
    const stopResponse = await request.post(`/api/v1/environments/${environmentId}/stop`, {
      headers: await csrfHeaders(request, baseURL, {
        'Idempotency-Key': uuidv7(),
        'If-Match': `"rev-${current.revision}"`,
      }),
    })
    const accepted = await expectJson(stopResponse, 'LAB_EXPERIMENT_ENVIRONMENT_STOP_FAILED')
    await pollJson(
      request,
      accepted.statusUrl,
      (value) => ['succeeded', 'failed', 'cancelled'].includes(value.state),
      'LAB_EXPERIMENT_ENVIRONMENT_STOP_STATUS_FAILED',
      240_000,
    )
    current = await waitForCleanupEnvironmentState(request, environmentId)
  } else if (['stopping', 'expiring', 'deleting'].includes(current.observedState)) {
    current = await waitForCleanupEnvironmentState(request, environmentId)
  }
  if (current.observedState === 'deleted') return
  const latestResponse = await request.get(`/api/v1/environments/${environmentId}`)
  if (latestResponse.status() === 404) return
  current = await expectJson(latestResponse, 'LAB_EXPERIMENT_ENVIRONMENT_READ_BEFORE_DELETE_FAILED')
  if (current.observedState === 'deleted') return
  const deleteResponse = await request.delete(`/api/v1/environments/${environmentId}`, {
    headers: await csrfHeaders(request, baseURL, {
      'Idempotency-Key': uuidv7(),
      'If-Match': `"rev-${current.revision}"`,
    }),
  })
  const accepted = await expectJson(deleteResponse, 'LAB_EXPERIMENT_ENVIRONMENT_DELETE_FAILED')
  const operation = await pollJson(
    request,
    accepted.statusUrl,
    (value) => ['succeeded', 'failed', 'cancelled'].includes(value.state),
    'LAB_EXPERIMENT_ENVIRONMENT_DELETE_STATUS_FAILED',
    240_000,
  )
  if (operation.state !== 'succeeded') {
    throw new Error(`LAB_EXPERIMENT_ENVIRONMENT_DELETE_OPERATION_FAILED:${operation.state}`)
  }
  current = await waitForCleanupEnvironmentState(request, environmentId)
  if (current.observedState !== 'deleted') {
    throw new Error(`LAB_EXPERIMENT_ENVIRONMENT_NOT_DELETED:${current.observedState}`)
  }
}

test('student completes a published lab experiment through the browser terminal', async ({ browser, page, baseURL }, testInfo) => {
  test.setTimeout(1_800_000)
  const request = page.context().request
  const teacherGuards = installUsabilityGuards(page)
  const resumeProjectId = process.env.LABWEAVER_E2E_LAB_RESUME_PROJECT_ID?.trim() ?? ''
  const resumeRunId = process.env.LABWEAVER_E2E_LAB_RESUME_RUN_ID?.trim() ?? ''
  if (Boolean(resumeProjectId) !== Boolean(resumeRunId)) {
    throw new Error('LAB_EXPERIMENT_RESUME_TARGET_INCOMPLETE')
  }
  const resumeExistingRun = Boolean(resumeProjectId && resumeRunId)
  const packageCopy = resumeExistingRun ? null : await mkdtemp(join(tmpdir(), 'labweaver-lab-'))
  let environmentId
  let studentContext
  let adminContext
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
      if (!runIsFullyTerminal(run)) {
        run = await waitForTerminalExperimentRun(
          request,
          project.id,
          run.id,
          () => approveAuthoringResourceRequestsByUi(adminPage, project.id, run.id, teacherActorId),
        )
      }
      let retryWaitOptions = {}
      if (isFirstFailedEnvironmentAttempt(run)) {
        const acceptedRetry = await retryFailedEnvironmentTrackByUi(page, project.id, run)
        retryWaitOptions = {
          minimumRevision: acceptedRetry.revision,
          requiredEnvironmentAttemptNumber: 2,
        }
      } else if (environmentRetryAlreadyUsed(run)) {
        throw new Error('LAB_EXPERIMENT_RESUME_ENVIRONMENT_RETRY_ALREADY_USED')
      }
      completed = await waitForExperimentRun(
        request,
        project.id,
        run.id,
        () => approveAuthoringResourceRequestsByUi(adminPage, project.id, run.id, teacherActorId),
        retryWaitOptions,
      )
    } else {
      await cp(LAB.root, packageCopy, { recursive: true })
      await configureLabPackageCopy(packageCopy)
      if (process.env.LABWEAVER_E2E_LAB === 'xv6') {
        const sourcePath = join(packageCopy, LAB.frozenPath)
        const source = await readFile(sourcePath, 'utf8')
        const starterSource = source.replace('xv6-student: hello', LAB.starterOutput)
        if (starterSource === source) throw new Error('LAB_EXPERIMENT_XV6_STARTER_OUTPUT_NOT_FOUND')
        await writeFile(sourcePath, starterSource, 'utf8')
      }
      project = await createProjectByUi(page, `real-${process.env.LABWEAVER_E2E_LAB}-${Date.now()}-${uuidv7().slice(0, 8)}`)
      await selectProjectByUi(page, project.id)
      await configureProjectPolicyByUi(page, project.id)
      await page.goto(`/teacher/materials?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
      await selectProjectByUi(page, project.id)
      packageData = await uploadPackageDirectoryByUi(page, packageCopy, LAB.frozenPath)
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
      () => approveAuthoringResourceRequestsByUi(adminPage, project.id, completed.run.id, teacherActorId),
    )
    const published = await approveAndPublish(
      page,
      project.id,
      completed.run.id,
      packageData,
      completed.environmentCandidateId,
      completed.evaluationCandidateId,
      built.artifact,
    )
    await assertNoStuckProgress(page, 'teacher-approval')
    await auditAccessibility(page, 'teacher-approval', testInfo)
    teacherGuards.assertCleanConsole('teacher-approval')

    studentContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.student })
    const studentPage = await studentContext.newPage()
    const studentGuards = installUsabilityGuards(studentPage)
    const studentActorId = await readActorId(studentContext.request)
    await addProjectStudentByUi(page, project.id, studentActorId)
    environmentId = await createEnvironmentByStudentUi(studentPage, project.id, published.publication.environmentReleaseId)
    const environment = await waitForStudentEnvironmentWithResourceApproval(
      studentContext.request,
      adminPage,
      project.id,
      environmentId,
      studentActorId,
    )
    if (LAB.gpuMode) {
      expect(environment.gpuAllocation?.mode, 'LAB_EXPERIMENT_GPU_MODE_MISSING').toBe(LAB.gpuMode)
      expect(environment.gpuAllocation?.class, 'LAB_EXPERIMENT_GPU_CLASS_MISSING').toBe(LAB.gpuClass)
    }
    await assertNoStuckProgress(studentPage, 'student-environment')
    await auditAccessibility(studentPage, 'student-environment', testInfo)
    const beforeRequestIds = await snapshotProjectResourceRequestIds(adminPage.request, project.id)

    const terminal = await issueAccessGrantAndConnect(studentPage, project.id, environmentId)
    const frames = []
    studentPage.on('websocket', (socket) => {
      socket.on('framereceived', ({ payload }) => {
        frames.push(typeof payload === 'string' ? payload : Buffer.from(payload).toString('utf8'))
      })
    })
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

    if (LAB.fixCommands.length > 0) {
      await issueAccessGrantAndConnect(studentPage, project.id, environmentId)
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
  } finally {
    try {
      if (environmentId) {
        const cleanupContext = studentContext ?? await browser.newContext({ baseURL, storageState: AUTH_STATE.student })
        const cleanupPage = await cleanupContext.newPage()
        await closeExperimentEnvironment(cleanupPage, environmentId, baseURL).catch(() => {})
        if (!studentContext) await cleanupContext.close()
      }
    } finally {
      await studentContext?.close()
      await adminContext?.close()
      if (packageCopy) await rm(packageCopy, { recursive: true, force: true })
    }
  }
})
