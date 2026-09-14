import { createHash } from 'node:crypto'
import { cp, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect } from '@playwright/test'
import {
  expectJson,
  pollJson,
  selectProjectByUi,
} from './live.mjs'

const PACKAGE_ROOT = join(dirname(fileURLToPath(import.meta.url)), '../../../examples/security-controlled')
const EVALUATION_RESOURCE_PROVIDER_BINDING = 'kubernetes-work-local-hostpath'
const EVALUATION_RESOURCE_APPROVAL_REASON = 'teacher real experiment task resource approval'
const RESOURCE_REQUEST_TERMINAL_STATES = new Set(['expired', 'rejected', 'cancelled'])
const RESOURCE_APPROVAL_POLL_INTERVAL_MS = 1000

/**
 * Resolve the explicit opt-in settings for a billable provider run.
 *
 * The test is intentionally unusable with a fixture/default model or a mutable
 * image tag. This keeps ordinary Playwright runs local and makes the external
 * dependency boundary visible when somebody elects to run this test.
 */
export function realProviderConfig() {
  if (process.env.LABWEAVER_E2E_REAL_PROVIDER !== '1') return null
  const model = process.env.LABWEAVER_E2E_PROVIDER_MODEL?.trim() ?? ''
  const goldenBaseImage = process.env.LABWEAVER_E2E_SECURITY_BASE_IMAGE?.trim() ?? ''
  if (!model || /\s/.test(model)) throw new Error('LABWEAVER_E2E_PROVIDER_MODEL_REQUIRED')
  const match = goldenBaseImage.match(/^([^\s@]+)@sha256:([0-9a-f]{64})$/i)
  if (!match) throw new Error('LABWEAVER_E2E_SECURITY_BASE_IMAGE_MUST_BE_DIGEST_PINNED')
  return Object.freeze({
    model,
    goldenBaseImage,
    goldenBaseDigest: `sha256:${match[2].toLowerCase()}`,
  })
}

/**
 * Read the explicit, non-generating continuation settings for the student leg.
 *
 * A project and authoring approval are the smallest public read context: the
 * approval projection exposes both candidate identities and the exact release
 * pair. A run ID alone cannot identify that approval because the public API has
 * no project-scoped approval listing endpoint.
 */
export function realExperimentResumeConfig() {
  const projectId = process.env.LABWEAVER_E2E_RESUME_PROJECT_ID?.trim() ?? ''
  const approvalId = process.env.LABWEAVER_E2E_RESUME_APPROVAL_ID?.trim() ?? ''
  const expectedAgentRunId = process.env.LABWEAVER_E2E_RESUME_AGENT_RUN_ID?.trim() ?? ''
  if (!projectId && !approvalId && !expectedAgentRunId) return null
  if (!projectId || !approvalId) {
    throw new Error('LABWEAVER_E2E_RESUME_PROJECT_AND_APPROVAL_REQUIRED')
  }
  return Object.freeze({
    projectId,
    approvalId,
    expectedAgentRunId: expectedAgentRunId || null,
  })
}

/**
 * Create a transient upload directory whose generated Dockerfile starts from
 * the caller-supplied private Harbor golden base. The source package remains
 * untouched; the manifest digest is updated in the temporary copy so the
 * package remains self-consistent when the normal package validator is used.
 */
export async function createSecurityControlledPackage(goldenBaseImage) {
  if (typeof goldenBaseImage !== 'string' || !/^([^\s@]+)@sha256:[0-9a-f]{64}$/i.test(goldenBaseImage)) {
    throw new Error('LABWEAVER_E2E_SECURITY_BASE_IMAGE_MUST_BE_DIGEST_PINNED')
  }
  const directory = await mkdtemp(join(tmpdir(), 'labweaver-security-controlled-'))
  try {
    const names = await readdir(PACKAGE_ROOT)
    for (const name of names) {
      await cp(join(PACKAGE_ROOT, name), join(directory, name), { recursive: true })
    }

    const dockerfilePath = join(directory, 'Dockerfile')
    const rewrittenDockerfile = [
      `FROM ${goldenBaseImage}`,
      '',
      'USER 0',
      'COPY student /opt/labweaver/student',
      'COPY reference /opt/labweaver/reference',
      'COPY tests /opt/labweaver/tests',
      'COPY scripts /opt/labweaver/scripts',
      'COPY README.md /opt/labweaver/workspace-seed/README.md',
      'COPY profiles /opt/labweaver/workspace-seed/profiles',
      'COPY student /opt/labweaver/workspace-seed/student',
      'COPY tests /opt/labweaver/workspace-seed/tests',
      'COPY workspace-seed/real-experiment-marker.txt /opt/labweaver/workspace-seed/real-experiment-marker.txt',
      '',
      'RUN mkdir -p /work /workspace /tmp',
      '    && chmod 0755 /opt/labweaver/scripts/local-test.sh',
      '    && chmod -R a+rX /opt/labweaver/workspace-seed',
      '    && chmod 0777 /work /workspace /tmp',
      '',
      'USER 65534:65534',
      'WORKDIR /workspace',
      'CMD ["/usr/bin/python3", "-m", "http.server", "8080", "--bind", "0.0.0.0", "--directory", "/workspace"]',
      '',
    ].join('\n')
    await writeFile(dockerfilePath, rewrittenDockerfile, 'utf8')

    const manifestPath = join(directory, 'manifest.json')
    const manifest = JSON.parse(await readFile(manifestPath, 'utf8'))
    const dockerfileEntry = manifest.spec?.files?.find((entry) => entry.path === 'Dockerfile')
    if (!dockerfileEntry) throw new Error('LABWEAVER_E2E_MANIFEST_DOCKERFILE_ENTRY_MISSING')
    dockerfileEntry.sha256 = createHash('sha256').update(rewrittenDockerfile).digest('hex')
    await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8')

    return {
      directory,
      async cleanup() {
        await rm(directory, { recursive: true, force: true })
      },
    }
  } catch (error) {
    await rm(directory, { recursive: true, force: true })
    throw error
  }
}

export async function uploadPackageDirectoryByUi(page, directory) {
  const fileInput = page.locator('input[type=file][webkitdirectory]')
  await fileInput.setInputFiles(directory)
  const fileRegion = page.getByRole('region', { name: '待上传材料文件', exact: true })
  await expect(fileRegion).toContainText('student/auth.c')
  const visiblePaths = new Set(await fileRegion.locator('.file-path').allTextContents())
  const manifest = JSON.parse(await readFile(join(directory, 'manifest.json'), 'utf8'))
  const expectedPaths = [
    'manifest.json',
    ...(manifest.spec?.files ?? []).map((entry) => entry.path),
  ]
  for (const expectedPath of expectedPaths) {
    expect(visiblePaths, `directory upload omitted ${expectedPath}`).toContain(expectedPath)
  }
  const directoryPrefix = `${basename(directory)}/`
  expect([...visiblePaths].some((path) => path.startsWith(directoryPrefix))).toBe(false)

  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && /\/api\/v1\/projects\/[^/]+\/problem-package-uploads\/[^/]+\/complete$/.test(url.pathname)
  })
  await page.getByRole('button', { name: '上传材料包', exact: true }).click()
  const response = await responsePromise
  const packageData = await expectJson(response, 'REAL_EXPERIMENT_PACKAGE_UPLOAD_FAILED')
  expect(packageData).toMatchObject({ projectId: expect.any(String), revision: expect.any(Number) })
  return packageData
}

export async function startExperimentRunByUi(page, projectId) {
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/projects/${projectId}/agent-runs`
  })
  await page.getByRole('button', { name: '启动 AgentRun', exact: true }).click()
  const run = await expectJson(await responsePromise, 'REAL_EXPERIMENT_AGENT_RUN_CREATE_FAILED')
  expect(run).toMatchObject({
    id: expect.any(String),
    projectId,
    purpose: { kind: 'authoring', environmentClass: 'experiment' },
  })
  return run
}

export async function readActorId(request) {
  const body = await expectJson(await request.get('/api/v1/auth/session'), 'REAL_EXPERIMENT_SESSION_LOOKUP_FAILED')
  const actorId = body.actor?.actorId
  if (typeof actorId !== 'string' || actorId.length === 0) throw new Error('REAL_EXPERIMENT_ACTOR_ID_MISSING')
  return actorId
}

export async function addProjectStudentByUi(page, projectId, actorId) {
  await page.goto(`/researcher/workspaces?projectId=${encodeURIComponent(projectId)}`, { waitUntil: 'domcontentloaded' })
  await selectProjectByUi(page, projectId)
  await page.getByRole('heading', { name: '项目成员', exact: true }).waitFor()
  const memberForm = page.locator('form.member-form')
  await expect(memberForm).toBeVisible()
  await memberForm.getByLabel('Actor ID', { exact: true }).fill(actorId)
  await memberForm.getByRole('combobox', { name: '角色', exact: true }).selectOption('student')
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/projects/${projectId}/members`
  })
  await page.getByRole('button', { name: '添加成员', exact: true }).click()
  const response = await responsePromise
  const member = await expectJson(response, 'REAL_EXPERIMENT_STUDENT_MEMBERSHIP_FAILED')
  expect(member).toMatchObject({ projectId, actorId, role: 'student', state: 'active' })
  return member
}

export async function waitForEnvironment(request, environmentId, expectedState = 'ready', timeout = 600_000) {
  return await pollJson(
    request,
    `/api/v1/environments/${environmentId}`,
    (value) => value.observedState === expectedState,
    `REAL_EXPERIMENT_ENVIRONMENT_${expectedState.toUpperCase()}_TIMEOUT`,
    timeout,
  )
}

export async function waitForFrozenSubmission(request, projectId, submissionId, timeout = 300_000) {
  let latest
  let failure
  await expect.poll(async () => {
    const response = await request.get(`/api/v1/projects/${projectId}/frozen-submissions/${submissionId}`)
    if (response.status() === 404) return false
    const body = await response.text()
    try {
      latest = JSON.parse(body)
    } catch (error) {
      failure = new Error('REAL_EXPERIMENT_FROZEN_SUBMISSION_INVALID_JSON', { cause: error })
      return true
    }
    if (!response.ok()) {
      failure = new Error(`REAL_EXPERIMENT_FROZEN_SUBMISSION_READ_FAILED:${response.status()}`)
      return true
    }
    return latest?.id === submissionId && Array.isArray(latest.files)
      && latest.files.some((file) => file.path === 'student/auth.c')
  }, { timeout, intervals: [1000, 2000, 3000] }).toBe(true)
  if (failure) throw failure
  return latest
}

export async function freezeStudentSourceByUi(page, projectId, environmentId) {
  const currentUrl = new URL(page.url())
  if (
    currentUrl.pathname !== '/student/environments'
    || currentUrl.searchParams.get('projectId') !== projectId
    || currentUrl.searchParams.get('environmentId') !== environmentId
  ) {
    await page.goto(`/student/environments?projectId=${encodeURIComponent(projectId)}&environmentId=${encodeURIComponent(environmentId)}`, {
      waitUntil: 'domcontentloaded',
    })
  }
  await expect(page.getByRole('heading', { name: environmentId, exact: true })).toBeVisible({ timeout: 120_000 })
  await page.getByRole('button', { name: '实验提交与凭据', exact: true }).click()
  const startButton = page.getByRole('button', { name: '发起冻结提交', exact: true })
  await expect(startButton).toBeEnabled({ timeout: 120_000 })
  await startButton.click()
  const dialog = page.getByRole('dialog', { name: '确认冻结清单', exact: true })
  await expect(dialog).toBeVisible()
  await expect(dialog.locator('.freeze-manifest')).toContainText('student/auth.c')
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/environments/${environmentId}/freeze`
  })
  await dialog.getByRole('button', { name: '确认冻结', exact: true }).click()
  const accepted = await expectJson(await responsePromise, 'REAL_EXPERIMENT_FREEZE_ACCEPT_FAILED')
  if (
    typeof accepted.operationId !== 'string'
    || accepted.operationId.length === 0
    || !Number.isInteger(accepted.revision)
    || accepted.revision < 1
    || typeof accepted.statusUrl !== 'string'
  ) {
    throw new Error('REAL_EXPERIMENT_FREEZE_ACCEPT_CONTRACT_INVALID')
  }
  const statusMatch = accepted.statusUrl.match(
    /^\/api\/v1\/projects\/([^/?#]+)\/frozen-submissions\/([0-9a-f-]{36})$/,
  )
  const submissionId = statusMatch?.[2]
  if (statusMatch?.[1] !== projectId) throw new Error('REAL_EXPERIMENT_FREEZE_STATUS_PROJECT_INVALID')
  if (!submissionId) throw new Error('REAL_EXPERIMENT_FREEZE_STATUS_URL_INVALID')
  const frozen = await waitForFrozenSubmission(page.request, projectId, submissionId)
  await expect(page.locator('.evidence-card')).toContainText(submissionId, { timeout: 120_000 })
  return frozen
}

async function readResourceRequests(request) {
  const response = await request.get('/api/v1/resource-requests')
  const body = await expectJson(response, 'REAL_EXPERIMENT_RESOURCE_REQUESTS_READ_FAILED')
  if (!Array.isArray(body)) throw new Error('REAL_EXPERIMENT_RESOURCE_REQUESTS_INVALID')
  return body
}

function isEvaluationTaskResourceRequest(request, projectId, studentActorId) {
  const taskRunId = request?.target?.taskRunId
  return request?.projectId === projectId
    && request?.requesterId === studentActorId
    && request?.target?.kind === 'task'
    && typeof taskRunId === 'string'
    && request.requestKey === `evaluation-${taskRunId}`
}

function resourceRequestLabel(request) {
  return `${request.id ?? 'id-missing'}:${request.requestKey ?? 'request-key-missing'}:${request.state ?? 'state-missing'}`
}

export async function assertNoPendingEvaluationTaskResourceRequests(request, projectId, studentActorId) {
  const requests = await readResourceRequests(request)
  const pending = requests.filter((item) =>
    isEvaluationTaskResourceRequest(item, projectId, studentActorId)
    && !RESOURCE_REQUEST_TERMINAL_STATES.has(item.state),
  )
  if (pending.length > 0) {
    throw new Error(
      `REAL_EXPERIMENT_PENDING_EVALUATION_RESOURCE_REQUESTS:${pending.map(resourceRequestLabel).join(',')}`,
    )
  }
}

export async function snapshotProjectResourceRequestIds(request, projectId) {
  const requests = await readResourceRequests(request)
  const ids = new Set()
  for (const item of requests) {
    if (item?.projectId !== projectId) continue
    if (typeof item.id !== 'string' || item.id.trim() === '') {
      throw new Error('REAL_EXPERIMENT_PROJECT_RESOURCE_REQUEST_ID_INVALID')
    }
    ids.add(item.id)
  }
  return ids
}

export async function approveEvaluationTaskResourceRequestByUi(page, request, projectId, studentActorId) {
  if (!isEvaluationTaskResourceRequest(request, projectId, studentActorId)) {
    throw new Error(`REAL_EXPERIMENT_RESOURCE_REQUEST_IDENTITY_INVALID:${resourceRequestLabel(request)}`)
  }
  if (request.state !== 'reviewing') {
    throw new Error(`REAL_EXPERIMENT_RESOURCE_REQUEST_NOT_REVIEWING:${resourceRequestLabel(request)}`)
  }

  const taskRunId = request.target.taskRunId
  await page.goto('/admin/resource-approval', { waitUntil: 'domcontentloaded' })
  const row = page.locator('tbody tr').filter({ hasText: request.requestKey })
  await expect(row).toHaveCount(1, { timeout: 120_000 })
  await expect(row).toContainText(`TaskRun ${taskRunId}`)
  await row.click()

  const detail = page.locator('.request-detail')
  await expect(detail).toBeVisible()
  await expect(detail).toContainText(request.id)
  await expect(detail).toContainText(projectId)
  await expect(detail).toContainText(studentActorId)
  await expect(detail).toContainText(`TaskRun ${taskRunId}`)

  const reason = detail.locator('textarea.reason-input')
  await expect(reason).toHaveCount(1)
  await reason.fill(EVALUATION_RESOURCE_APPROVAL_REASON)

  const provider = detail.locator('#provider-binding')
  await expect(provider).toHaveCount(1)
  if (await provider.evaluate((element) => element.tagName === 'SELECT')) {
    await provider.selectOption(EVALUATION_RESOURCE_PROVIDER_BINDING)
  } else {
    await provider.fill(EVALUATION_RESOURCE_PROVIDER_BINDING)
  }

  const duration = detail.locator('#approve-duration')
  await expect(duration).toHaveCount(1)
  if (!Number.isInteger(request.requestedDurationSeconds) || request.requestedDurationSeconds <= 0) {
    throw new Error(`REAL_EXPERIMENT_RESOURCE_REQUEST_DURATION_INVALID:${resourceRequestLabel(request)}`)
  }
  await duration.fill(String(request.requestedDurationSeconds))

  const approveButton = detail.locator('.approval-buttons .filled-button').first()
  await expect(approveButton).toHaveCount(1)
  await expect(approveButton).toBeEnabled()

  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/resource-requests/${request.id}/approve`
  })
  const interactionPromise = (async () => {
    await approveButton.click()
    const dialog = page.locator('dialog.confirm-dialog[role="alertdialog"]')
    await expect(dialog).toBeVisible()
    const confirmButton = dialog.locator('.filled-button')
    await expect(confirmButton).toHaveCount(1)
    await confirmButton.click()
  })()

  let response
  try {
    const responses = await Promise.all([responsePromise, interactionPromise])
    response = responses[0]
  } catch (error) {
    const settled = await Promise.allSettled([responsePromise, interactionPromise])
    const secondaryErrors = settled
      .filter((outcome) => outcome.status === 'rejected' && outcome.reason !== error)
      .map((outcome) => outcome.reason)
    if (secondaryErrors.length > 0) {
      throw new AggregateError(
        [error, ...secondaryErrors],
        'REAL_EXPERIMENT_RESOURCE_REQUEST_APPROVAL_UI_FAILED',
      )
    }
    throw error
  }

  const approval = await expectJson(response, 'REAL_EXPERIMENT_RESOURCE_REQUEST_APPROVAL_FAILED')
  if (
    approval?.requestId !== request.id
    || typeof approval.leaseId !== 'string'
    || approval.leaseId.trim() === ''
  ) {
    throw new Error(`REAL_EXPERIMENT_RESOURCE_REQUEST_APPROVAL_CONTRACT_INVALID:${request.id}`)
  }
  return approval
}

export async function waitForProjectEvaluationResultWithResourceApproval({
  request,
  adminPage,
  projectId,
  frozenSubmissionId,
  studentActorId,
  existingRequestIds,
  timeout = 600_000,
}) {
  if (!(existingRequestIds instanceof Set)) throw new Error('REAL_EXPERIMENT_RESOURCE_REQUEST_SNAPSHOT_REQUIRED')
  if (!adminPage?.request) throw new Error('REAL_EXPERIMENT_RESOURCE_APPROVAL_ADMIN_PAGE_REQUIRED')
  const approvedRequestIds = new Set()
  let latestResult
  let terminalError

  await expect.poll(async () => {
    if (terminalError) return true

    let resultBody
    try {
      const resultResponse = await request.get(`/api/v1/projects/${projectId}/me/evaluation-results`)
      resultBody = await expectJson(resultResponse, 'REAL_EXPERIMENT_EVALUATION_RESULTS_READ_FAILED')
    } catch (error) {
      terminalError = error instanceof Error
        ? error
        : new Error('REAL_EXPERIMENT_EVALUATION_RESULTS_READ_FAILED', { cause: error })
      return true
    }
    if (!Array.isArray(resultBody.items)) {
      terminalError = new Error('REAL_EXPERIMENT_EVALUATION_RESULTS_INVALID')
      return true
    }
    latestResult = resultBody.items.find((item) => item.frozenSubmissionId === frozenSubmissionId)
    if (latestResult && ['succeeded', 'failed', 'cancelled'].includes(latestResult.state)) {
      if (latestResult.state !== 'succeeded') {
        terminalError = new Error(`REAL_EXPERIMENT_EVALUATION_FAILED:${latestResult.state}`)
        return true
      }
      return true
    }

    let requests
    try {
      requests = await readResourceRequests(adminPage.request)
    } catch (error) {
      terminalError = error instanceof Error
        ? error
        : new Error('REAL_EXPERIMENT_RESOURCE_REQUESTS_READ_FAILED', { cause: error })
      return true
    }
    const candidates = requests.filter((item) =>
      isEvaluationTaskResourceRequest(item, projectId, studentActorId)
      && typeof item.id === 'string'
      && !existingRequestIds.has(item.id)
      && !approvedRequestIds.has(item.id)
      && item.state === 'reviewing',
    )
    if (candidates.length > 1) {
      terminalError = new Error(
        `REAL_EXPERIMENT_EVALUATION_RESOURCE_REQUEST_AMBIGUOUS:${candidates.map(resourceRequestLabel).join(',')}`,
      )
      return true
    }
    if (candidates.length === 1) {
      const candidate = candidates[0]
      try {
        await approveEvaluationTaskResourceRequestByUi(
          adminPage,
          candidate,
          projectId,
          studentActorId,
        )
      } catch (error) {
        terminalError = error instanceof Error
          ? error
          : new Error(`REAL_EXPERIMENT_RESOURCE_REQUEST_APPROVAL_FAILED:${candidate.id}`, { cause: error })
        return true
      }
      approvedRequestIds.add(candidate.id)
    }
    return false
  }, {
    timeout,
    intervals: [RESOURCE_APPROVAL_POLL_INTERVAL_MS, 2000, 3000],
    message: `等待评测结果或资源申请：${frozenSubmissionId}`,
  }).toBe(true)

  if (terminalError) throw terminalError
  if (latestResult?.state === 'succeeded') return latestResult
  throw new Error(
    `REAL_EXPERIMENT_EVALUATION_RESOURCE_APPROVAL_TIMEOUT:${frozenSubmissionId}:${latestResult?.state ?? 'result-missing'}`,
  )
}

export function containerArtifact(candidate) {
  const artifact = candidate?.imageArtifact ?? candidate?.build?.artifact
  if (!artifact || artifact.kind !== 'container') throw new Error('REAL_EXPERIMENT_CONTAINER_ARTIFACT_MISSING')
  if (typeof artifact.repository !== 'string' || artifact.repository.trim() === '') {
    throw new Error('REAL_EXPERIMENT_CONTAINER_REPOSITORY_MISSING')
  }
  if (!/^sha256:[0-9a-f]{64}$/i.test(artifact.digest ?? '')) {
    throw new Error('REAL_EXPERIMENT_CONTAINER_DIGEST_INVALID')
  }
  return artifact
}

export function assertGeneratedBuildUsesPrivateContext(candidate) {
  const runtime = candidate?.candidate?.spec?.runtime
  if (runtime?.kind !== 'container' || !runtime.build_context?.artifactId || !runtime.build_context.objectVersion) {
    throw new Error('REAL_EXPERIMENT_GENERATED_BUILD_CONTEXT_MISSING')
  }
  return runtime.build_context
}

function assertSameContainerArtifact(actual, expected, code) {
  if (
    actual?.kind !== 'container'
    || expected?.kind !== 'container'
    || actual.id !== expected.id
    || actual.repository !== expected.repository
    || actual.digest?.toLowerCase() !== expected.digest?.toLowerCase()
    || actual.build_request_id !== expected.build_request_id
  ) {
    throw new Error(code)
  }
  return actual
}

async function readProjectForResume(request, projectId) {
  const actorId = await readActorId(request)
  const project = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}`),
    'REAL_EXPERIMENT_RESUME_PROJECT_READ_FAILED',
  )
  if (project.id !== projectId || project.ownerActorId !== actorId) {
    throw new Error('REAL_EXPERIMENT_RESUME_PROJECT_OWNERSHIP_INVALID')
  }
  return project
}

/**
 * Read and validate a completed teacher publication before entering the
 * student-only continuation. No command or approval is created here.
 */
export async function readResumablePublishedExperiment(request, resume) {
  const { projectId, approvalId, expectedAgentRunId } = resume
  const project = await readProjectForResume(request, projectId)
  const publication = await expectJson(
    await request.get(
      `/api/v1/projects/${encodeURIComponent(projectId)}/authoring-approvals/${encodeURIComponent(approvalId)}`,
    ),
    'REAL_EXPERIMENT_RESUME_PUBLICATION_READ_FAILED',
  )
  if (
    publication.status !== 'ready'
    || !publication.approval
    || publication.approval?.id !== approvalId
    || publication.approval?.projectId !== projectId
    || typeof publication.environmentReleaseId !== 'string'
    || publication.environmentReleaseId.trim() === ''
    || typeof publication.evaluationReleaseId !== 'string'
    || publication.evaluationReleaseId.trim() === ''
    || !Number.isInteger(publication.evaluationReleaseRevision)
    || publication.evaluationReleaseRevision < 1
  ) {
    throw new Error('REAL_EXPERIMENT_RESUME_PUBLICATION_NOT_READY_OR_PAIR_INVALID')
  }

  const approval = publication.approval
  const environmentCandidateId = approval.environmentCandidateId
  const evaluationCandidateId = approval.evaluationCandidateId
  if (typeof environmentCandidateId !== 'string' || typeof evaluationCandidateId !== 'string') {
    throw new Error('REAL_EXPERIMENT_RESUME_APPROVAL_CANDIDATE_IDS_MISSING')
  }
  const [environmentCandidate, evaluationCandidate] = await Promise.all([
    expectJson(
      await request.get(
        `/api/v1/projects/${encodeURIComponent(projectId)}/environment-candidates/${encodeURIComponent(environmentCandidateId)}`,
      ),
      'REAL_EXPERIMENT_RESUME_ENVIRONMENT_CANDIDATE_READ_FAILED',
    ),
    expectJson(
      await request.get(
        `/api/v1/projects/${encodeURIComponent(projectId)}/evaluation-candidates/${encodeURIComponent(evaluationCandidateId)}`,
      ),
      'REAL_EXPERIMENT_RESUME_EVALUATION_CANDIDATE_READ_FAILED',
    ),
  ])
  const environment = environmentCandidate.candidate
  const evaluation = evaluationCandidate.candidate
  if (
    environment.id !== environmentCandidateId
    || environment.projectId !== projectId
    || approval.courseId !== environment.courseId
    || environment.spec?.class !== 'experiment'
    || environmentCandidate.build?.state !== 'succeeded'
    || approval.environmentCandidateRevision !== environment.revision
    || evaluation.id !== evaluationCandidateId
    || evaluation.projectId !== projectId
    || approval.courseId !== evaluation.courseId
    || approval.evaluationCandidateRevision !== evaluation.revision
  ) {
    throw new Error('REAL_EXPERIMENT_RESUME_CANDIDATE_INVALID')
  }
  const runId = environment.runId
  if (expectedAgentRunId && expectedAgentRunId !== runId) {
    throw new Error('REAL_EXPERIMENT_RESUME_AGENT_RUN_MISMATCH')
  }
  if (evaluation.runId !== runId) {
    throw new Error('REAL_EXPERIMENT_RESUME_CANDIDATE_RUN_PAIR_INVALID')
  }
  const run = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/agent-runs/${encodeURIComponent(runId)}`),
    'REAL_EXPERIMENT_RESUME_AGENT_RUN_READ_FAILED',
  )
  if (
    run.id !== runId
    || run.projectId !== projectId
    || run.state !== 'succeeded'
    || run.purpose?.kind !== 'authoring'
    || run.purpose.environmentClass !== 'experiment'
    || run.tracks?.find((track) => track.kind === 'environment')?.candidateId !== environmentCandidateId
    || run.tracks?.find((track) => track.kind === 'evaluation')?.candidateId !== evaluationCandidateId
  ) {
    throw new Error('REAL_EXPERIMENT_RESUME_AGENT_RUN_INVALID')
  }

  const builtArtifact = containerArtifact(environmentCandidate)
  const buildArtifact = environmentCandidate.build?.artifact
  assertSameContainerArtifact(builtArtifact, buildArtifact, 'REAL_EXPERIMENT_RESUME_BUILD_ARTIFACT_INVALID')
  assertSameContainerArtifact(builtArtifact, approval.imageArtifact, 'REAL_EXPERIMENT_RESUME_APPROVAL_ARTIFACT_INVALID')

  const environmentRelease = await expectJson(
    await request.get(
      `/api/v1/projects/${encodeURIComponent(projectId)}/environment-template-releases/${encodeURIComponent(publication.environmentReleaseId)}`,
    ),
    'REAL_EXPERIMENT_RESUME_ENVIRONMENT_RELEASE_READ_FAILED',
  )
  if (
    environmentRelease.id !== publication.environmentReleaseId
    || environmentRelease.projectId !== projectId
    || environmentRelease.courseId !== approval.courseId
    || environmentRelease.agentRunId !== runId
    || environmentRelease.candidateId !== environmentCandidateId
    || environmentRelease.candidateRevision !== environment.revision
    || environmentRelease.runtimeKind !== 'container'
    || environmentRelease.approval?.id !== approval.id
    || environmentRelease.approval?.candidateId !== environmentCandidateId
    || environmentRelease.approval?.candidateRevision !== environment.revision
  ) {
    throw new Error('REAL_EXPERIMENT_RESUME_RELEASE_PAIR_INVALID')
  }
  assertSameContainerArtifact(environmentRelease.artifact, approval.imageArtifact, 'REAL_EXPERIMENT_RESUME_RELEASE_ARTIFACT_INVALID')

  return {
    project,
    run,
    approval,
    publication,
    environmentCandidate,
    evaluationCandidate,
    environmentRelease,
  }
}
