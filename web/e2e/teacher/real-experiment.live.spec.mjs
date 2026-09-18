import { expect, test } from '@playwright/test'
import {
  AUTH_STATE,
  createProjectByUi,
  createProjectPolicy,
  csrfHeaders,
  expectJson,
  pollEnvironmentCandidate,
  pollJson,
  selectProjectByUi,
  uuidv7,
} from '../support/live.mjs'
import {
  addProjectStudentByUi,
  assertNoPendingEvaluationTaskResourceRequests,
  assertGeneratedBuildUsesPrivateContext,
  snapshotProjectResourceRequestIds,
  containerArtifact,
  createSecurityControlledPackage,
  freezeStudentSourceByUi,
  waitForProjectEvaluationResultWithResourceApproval,
  readActorId,
  readResumablePublishedExperiment,
  realProviderConfig,
  realExperimentResumeConfig,
  startExperimentRunByUi,
  uploadPackageDirectoryByUi,
  waitForEnvironment,
} from '../support/real-experiment.mjs'

const REAL_CHAIN_TIMEOUT_MS = 3_600_000
const REAL_PROVIDER_BUDGET = Object.freeze({
  maxOutputTokens: 32_000,
  timeoutMilliseconds: 300_000,
  maxTransientRetries: 0,
})
const config = realProviderConfig()
const resume = realExperimentResumeConfig()

test.describe.configure({ timeout: REAL_CHAIN_TIMEOUT_MS, retries: 0 })

function terminalRunState(value) {
  return ['succeeded', 'partially_succeeded', 'failed', 'cancelled'].includes(value)
}

function diagnosticCodes(run) {
  return run.tracks
    .flatMap((track) => track.attempts ?? [])
    .map((attempt) => attempt.diagnosticCode)
    .filter(Boolean)
    .join(',') || 'no attempt diagnostic'
}

function shellOctal(value) {
  return [...Buffer.from(value, 'utf8')]
    .map((byte) => `\\0${byte.toString(8).padStart(3, '0')}`)
    .join('')
}

async function waitForExperimentRun(request, projectId, runId) {
  const run = await pollJson(
    request,
    `/api/v1/projects/${projectId}/agent-runs/${runId}`,
    (value) => terminalRunState(value.state),
    'REAL_EXPERIMENT_AGENT_RUN_STATUS_FAILED',
    600_000,
  )
  if (run.state !== 'succeeded') throw new Error(`REAL_EXPERIMENT_AGENT_RUN_FAILED:${run.state}:${diagnosticCodes(run)}`)
  const environment = run.tracks.find((track) => track.kind === 'environment')
  const evaluation = run.tracks.find((track) => track.kind === 'evaluation')
  if (!environment?.candidateId || !evaluation?.candidateId) throw new Error('REAL_EXPERIMENT_CANDIDATES_MISSING')
  return { run, environmentCandidateId: environment.candidateId, evaluationCandidateId: evaluation.candidateId }
}

async function waitForBuiltCandidate(request, projectId, candidateId) {
  const candidate = await pollEnvironmentCandidate(
    request,
    projectId,
    candidateId,
    (value) => ['succeeded', 'failed', 'cancelled'].includes(value.build?.state),
    'REAL_EXPERIMENT_CANDIDATE_BUILD_STATUS_FAILED',
    600_000,
  )
  if (candidate.build?.state !== 'succeeded') {
    throw new Error(`REAL_EXPERIMENT_CANDIDATE_BUILD_FAILED:${candidate.build?.diagnosticCode ?? 'diagnostic missing'}`)
  }
  const artifact = containerArtifact(candidate)
  if (artifact.digest.toLowerCase() === config.goldenBaseDigest) throw new Error('REAL_EXPERIMENT_BUILD_REUSED_GOLDEN_BASE_DIGEST')
  const buildContext = assertGeneratedBuildUsesPrivateContext(candidate)
  return { candidate, artifact, buildContext }
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
  await page.locator('textarea.reason-input').fill('已核对真实生成的 Environment、Evaluation、私有黄金基础镜像来源和构建产物摘要。')
  await expect(approvalButton).toBeEnabled({ timeout: 240_000 })

  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/projects/${projectId}/authoring-approvals`
  })
  await approvalButton.click()
  const response = await responsePromise
  const approval = await expectJson(response, 'REAL_EXPERIMENT_APPROVAL_FAILED')
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
    'REAL_EXPERIMENT_PUBLICATION_STATUS_FAILED',
    600_000,
  )
  if (publication.status !== 'ready') throw new Error(`REAL_EXPERIMENT_PUBLICATION_FAILED:${publication.diagnosticCode ?? 'diagnostic missing'}`)
  expect(publication.environmentReleaseId).toEqual(expect.any(String))
  expect(publication.evaluationReleaseId).toEqual(expect.any(String))
  await expect(page.locator('.publication-status')).toHaveAttribute('data-status', 'ready', { timeout: 120_000 })
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
  const accepted = await expectJson(await responsePromise, 'REAL_EXPERIMENT_ENVIRONMENT_CREATE_FAILED')
  expect(accepted).toMatchObject({ environmentId: expect.any(String), operationId: expect.any(String) })
  return accepted.environmentId
}

async function issueTerminalAccessAndConnect(page, projectId, environmentId) {
  await page.goto(`/student/environments?projectId=${encodeURIComponent(projectId)}&environmentId=${encodeURIComponent(environmentId)}`, {
    waitUntil: 'domcontentloaded',
  })
  const environmentIdDetails = page.locator('details.environment-id-details')
  await expect(environmentIdDetails).toBeVisible({ timeout: 120_000 })
  await environmentIdDetails.locator('summary').click()
  await expect(environmentIdDetails.locator('code')).toHaveText(environmentId, { timeout: 30_000 })
  await expect(page.getByRole('button', { name: '概览与访问', exact: true })).toBeVisible()
  const grantButton = page.getByRole('button', { name: '签发访问授权', exact: true })
  const grantCard = page.locator('.grant-card')
  await expect.poll(
    async () => (await grantButton.count()) > 0 || (await grantCard.count()) > 0,
    { timeout: 120_000, intervals: [250, 500, 1000] },
  ).toBe(true)
  if (await grantButton.count() > 0) {
    await expect(grantButton).toBeEnabled({ timeout: 120_000 })
    const grantResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST' && url.pathname === `/api/v1/environments/${environmentId}/access-grants`
    })
    await grantButton.click()
    await expectJson(await grantResponsePromise, 'REAL_EXPERIMENT_ACCESS_GRANT_FAILED')
  }
  await pollJson(
    page.request,
    `/api/v1/environments/${environmentId}/access-grants?state=active&includeTerminal=false&limit=2`,
    (value) => Array.isArray(value.items) && value.items.length === 1,
    'REAL_EXPERIMENT_ACCESS_GRANT_ACTIVE_TIMEOUT',
    120_000,
  )
  const terminalFrames = []
  page.on('websocket', (socket) => {
    socket.on('framereceived', ({ payload }) => {
      terminalFrames.push(typeof payload === 'string' ? payload : Buffer.from(payload).toString('utf8'))
    })
  })
  await page.getByRole('button', { name: 'Web 控制台', exact: true }).click()
  const connectTerminal = page.getByRole('button', { name: '立即签发授权并连接终端', exact: true })
  if (await connectTerminal.count() > 0) {
    await expect(connectTerminal).toBeEnabled({ timeout: 120_000 })
    await connectTerminal.click()
  }
  const host = page.locator('.xterm-host')
  await expect(host).toBeVisible({ timeout: 120_000 })
  const input = page.locator('.xterm-helper-textarea')
  await expect(input).toBeVisible({ timeout: 30_000 })
  return { input, terminalFrames }
}

async function editThroughTerminal({ page, input, terminalFrames }) {
  await page.getByRole('button', { name: 'Web 控制台', exact: true }).click()
  await expect(page.locator('.xterm-host')).toBeVisible({ timeout: 120_000 })
  await expect(input).toBeVisible({ timeout: 30_000 })
  await input.focus()
  await page.keyboard.type("sed -i 's/password_length == 0/password_length != sizeof(expected_password) - 1/' student/auth.c")
  await page.keyboard.press('Enter')
  const completionMarker = 'LABWEAVER_DONE_7d9b'
  const matchPrefix = 'LABWEAVER_MATCH:'
  await page.keyboard.type([
    "if grep -q 'password_length !=' student/auth.c; then",
    `printf '%b\\n' '${shellOctal(completionMarker)}';`,
    `grep -n 'password_length !=' student/auth.c | while IFS= read -r line; do printf '%b%s\\n' '${shellOctal(matchPrefix)}' "$line"; done;`,
    'else exit 1; fi',
  ].join(' '))
  await page.keyboard.press('Enter')
  await expect.poll(
    () => terminalFrames.join(''),
    { timeout: 30_000, intervals: [250, 500, 1000] },
  ).toContain(completionMarker)
  await expect.poll(
    () => terminalFrames.join(''),
    { timeout: 30_000, intervals: [250, 500, 1000] },
  ).toContain(matchPrefix)
}

async function waitForCleanupEnvironmentState(request, environmentId) {
  let latest
  await expect.poll(async () => {
    const response = await request.get(`/api/v1/environments/${environmentId}`)
    if (response.status() === 404) {
      latest = { observedState: 'deleted' }
      return true
    }
    latest = await expectJson(response, 'REAL_EXPERIMENT_ENVIRONMENT_CLEANUP_READ_FAILED')
    return ['stopped', 'failed', 'deleting', 'deleted'].includes(latest.observedState)
  }, { timeout: 240_000, intervals: [1000, 2000, 3000] }).toBe(true)
  return latest
}

async function revokeEnvironmentAccessGrants(request, baseURL, environmentId) {
  const listResponse = await request.get(`/api/v1/environments/${environmentId}/access-grants?includeTerminal=false&limit=100`)
  if (listResponse.status() === 404) return
  const listed = await expectJson(listResponse, 'REAL_EXPERIMENT_ACCESS_GRANTS_CLEANUP_LIST_FAILED')
  if (!Array.isArray(listed.items)) throw new Error('REAL_EXPERIMENT_ACCESS_GRANTS_CLEANUP_LIST_INVALID')
  for (const item of listed.items) {
    let grantResponse = await request.get(`/api/v1/access-grants/${item.id}`)
    if (grantResponse.status() === 404) continue
    let grant = await expectJson(grantResponse, 'REAL_EXPERIMENT_ACCESS_GRANT_CLEANUP_READ_FAILED')
    if (!['requested', 'active'].includes(grant.state)) continue
    const revokeResponse = await request.post(`/api/v1/access-grants/${grant.id}/revoke`, {
      headers: await csrfHeaders(request, baseURL, {
        'Idempotency-Key': uuidv7(),
        'If-Match': `"rev-${grant.revision}"`,
      }),
      data: { grantId: grant.id, reasonCode: 'real_experiment_cleanup' },
    })
    const revoked = await expectJson(revokeResponse, 'REAL_EXPERIMENT_ACCESS_GRANT_CLEANUP_REVOKE_FAILED')
    expect(revoked).toMatchObject({ id: grant.id, state: 'revoked' })
    grant = await pollJson(
      request,
      `/api/v1/access-grants/${grant.id}`,
      (value) => ['revoked', 'denied', 'expired'].includes(value.state),
      'REAL_EXPERIMENT_ACCESS_GRANT_CLEANUP_STATUS_FAILED',
      120_000,
    )
    if (grant.state !== 'revoked') throw new Error(`REAL_EXPERIMENT_ACCESS_GRANT_NOT_REVOKED:${grant.state}`)
  }
}

async function closeExperimentEnvironment(page, projectId, environmentId, baseURL) {
  const request = page.request
  await revokeEnvironmentAccessGrants(request, baseURL, environmentId)
  const currentResponse = await request.get(`/api/v1/environments/${environmentId}`)
  if (currentResponse.status() === 404) return
  let current = await expectJson(currentResponse, 'REAL_EXPERIMENT_ENVIRONMENT_READ_FOR_CLEANUP_FAILED')
  if (current.observedState === 'deleted') return

  if (current.observedState === 'ready') {
    const stopResponse = await request.post(`/api/v1/environments/${environmentId}/stop`, {
      headers: await csrfHeaders(request, baseURL, {
        'Idempotency-Key': uuidv7(),
        'If-Match': `"rev-${current.revision}"`,
      }),
    })
    const accepted = await expectJson(stopResponse, 'REAL_EXPERIMENT_ENVIRONMENT_STOP_FAILED')
    const operation = await pollJson(
      request,
      accepted.statusUrl,
      (value) => ['succeeded', 'failed', 'cancelled'].includes(value.state),
      'REAL_EXPERIMENT_ENVIRONMENT_STOP_STATUS_FAILED',
      240_000,
    )
    if (operation.state !== 'succeeded') {
      current = await waitForCleanupEnvironmentState(request, environmentId)
      if (!['stopped', 'failed', 'deleting', 'deleted'].includes(current.observedState)) {
        throw new Error(`REAL_EXPERIMENT_ENVIRONMENT_STOP_OPERATION_FAILED:${operation.state}`)
      }
    } else {
      current = await waitForCleanupEnvironmentState(request, environmentId)
    }
  } else if (['stopping', 'expiring'].includes(current.observedState)) {
    current = await waitForCleanupEnvironmentState(request, environmentId)
  }

  if (current.observedState === 'deleted') return
  if (current.observedState === 'deleting') {
    await waitForCleanupEnvironmentState(request, environmentId)
    return
  }

  const latestResponse = await request.get(`/api/v1/environments/${environmentId}`)
  if (latestResponse.status() === 404) return
  current = await expectJson(latestResponse, 'REAL_EXPERIMENT_ENVIRONMENT_READ_BEFORE_DELETE_FAILED')
  if (current.observedState === 'deleted') return
  if (current.observedState === 'deleting') {
    await waitForCleanupEnvironmentState(request, environmentId)
    return
  }
  const deleteResponse = await request.delete(`/api/v1/environments/${environmentId}`, {
    headers: await csrfHeaders(request, baseURL, {
      'Idempotency-Key': uuidv7(),
      'If-Match': `"rev-${current.revision}"`,
    }),
  })
  const accepted = await expectJson(deleteResponse, 'REAL_EXPERIMENT_ENVIRONMENT_DELETE_FAILED')
  const operation = await pollJson(
    request,
    accepted.statusUrl,
    (value) => ['succeeded', 'failed', 'cancelled'].includes(value.state),
    'REAL_EXPERIMENT_ENVIRONMENT_DELETE_STATUS_FAILED',
    240_000,
  )
  if (operation.state !== 'succeeded') throw new Error(`REAL_EXPERIMENT_ENVIRONMENT_DELETE_OPERATION_FAILED:${operation.state}`)
  current = await waitForCleanupEnvironmentState(request, environmentId)
  if (current.observedState !== 'deleted') throw new Error(`REAL_EXPERIMENT_ENVIRONMENT_NOT_DELETED:${current.observedState}`)
}

async function continueStudentAcceptance({ browser, teacherPage, baseURL, projectId, publication, testInfo }) {
  let studentContext
  let studentPage
  let adminContext
  let adminPage
  let environmentId
  let primaryError
  let hasPrimaryError = false
  try {
    studentContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.student })
    studentPage = await studentContext.newPage()
    const studentActorId = await readActorId(studentContext.request)
    await addProjectStudentByUi(teacherPage, projectId, studentActorId)
    adminContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
    adminPage = await adminContext.newPage()
    await assertNoPendingEvaluationTaskResourceRequests(adminPage.request, projectId, studentActorId)

    environmentId = await createEnvironmentByStudentUi(
      studentPage,
      projectId,
      publication.environmentReleaseId,
    )
    await waitForEnvironment(studentContext.request, environmentId)
    await issueTerminalAccessAndConnect(studentPage, projectId, environmentId)
    const beforeRequestIds = await snapshotProjectResourceRequestIds(adminPage.request, projectId)
    const before = await freezeStudentSourceByUi(studentPage, projectId, environmentId)
    const beforeResult = await waitForProjectEvaluationResultWithResourceApproval({
      request: studentContext.request,
      adminPage,
      projectId,
      frozenSubmissionId: before.id,
      studentActorId,
      existingRequestIds: beforeRequestIds,
    })
    expect(beforeResult.awardedScore).toBeLessThan(beforeResult.maxScore)

    const terminal = await issueTerminalAccessAndConnect(studentPage, projectId, environmentId)
    await editThroughTerminal({ page: studentPage, ...terminal })
    await studentPage.screenshot({ path: testInfo.outputPath('environment-terminal.png'), fullPage: true })
    await assertNoPendingEvaluationTaskResourceRequests(adminPage.request, projectId, studentActorId)
    const afterRequestIds = await snapshotProjectResourceRequestIds(adminPage.request, projectId)
    const after = await freezeStudentSourceByUi(studentPage, projectId, environmentId)
    const afterResult = await waitForProjectEvaluationResultWithResourceApproval({
      request: studentContext.request,
      adminPage,
      projectId,
      frozenSubmissionId: after.id,
      studentActorId,
      existingRequestIds: afterRequestIds,
    })
    expect(afterResult.awardedScore).toBeGreaterThan(beforeResult.awardedScore)
    expect(afterResult.awardedScore).toBe(afterResult.maxScore)
    const advisory = afterResult.steps.find((step) => step.role === 'advisory')
    expect(advisory).toMatchObject({
      role: 'advisory',
      state: 'succeeded',
      review: {
        schema_version: 'goal-review/v1',
        assessment: expect.stringMatching(/^(met|partially_met|not_met|insufficient_evidence)$/),
        confidence: expect.any(Number),
        requires_teacher_attention: expect.any(Boolean),
      },
    })
    expect(advisory.review.confidence).toBeGreaterThanOrEqual(0)
    expect(advisory.review.confidence).toBeLessThanOrEqual(1)

    const resultsLink = studentPage
      .locator('.freeze-status-actions')
      .getByRole('link', { name: '\u67e5\u770b\u8bc4\u6d4b\u7ed3\u679c', exact: true })
    await expect(resultsLink).toHaveCount(1, { timeout: 120_000 })
    await resultsLink.click()
    await expect(studentPage).toHaveURL(
      (url) => url.pathname === '/student/results' && url.searchParams.get('projectId') === projectId,
    )
    await expect(studentPage.getByRole('heading', { name: '\u8bc4\u6d4b\u7ed3\u679c', exact: true })).toBeVisible()
    const afterResultCard = studentPage.locator('.result-card').filter({ hasText: afterResult.runId })
    await expect(afterResultCard).toHaveCount(1, { timeout: 120_000 })
    await expect(afterResultCard.locator('.state-chip--succeeded')).toHaveCount(1)
    await expect(afterResultCard.locator('.result-score')).toHaveText(`${afterResult.awardedScore} / ${afterResult.maxScore}`)
    await expect(afterResultCard.locator('.result-time')).toBeVisible()
    const resultLink = afterResultCard.locator('.result-link')
    await expect(resultLink).toHaveText(afterResult.runId)
    await resultLink.click()
    await expect(studentPage).toHaveURL(
      (url) => url.pathname === `/student/results/${afterResult.runId}`
        && url.searchParams.get('projectId') === projectId,
    )
    await expect(studentPage.getByRole('heading', { name: '\u8bc4\u6d4b\u8be6\u60c5', exact: true })).toBeVisible()
    await expect(studentPage.locator('.goal-review')).toBeVisible({ timeout: 120_000 })
    await studentPage.locator('a.back-link').click()
    await expect(studentPage).toHaveURL(
      (url) => url.pathname === '/student/results' && url.searchParams.get('projectId') === projectId,
    )
    await studentPage.screenshot({ path: testInfo.outputPath('result.png'), fullPage: true })
  } catch (error) {
    hasPrimaryError = true
    primaryError = error
  }

  const cleanupErrors = []
  try {
    if (studentPage && environmentId) await closeExperimentEnvironment(studentPage, projectId, environmentId, baseURL)
  } catch (error) {
    cleanupErrors.push(error)
  }
  try {
    await studentContext?.close()
  } catch (error) {
    cleanupErrors.push(error)
  }
  try {
    await adminContext?.close()
  } catch (error) {
    cleanupErrors.push(error)
  }

  if (hasPrimaryError && cleanupErrors.length > 0) {
    throw new AggregateError(
      [primaryError, ...cleanupErrors],
      'REAL_EXPERIMENT_STUDENT_ACCEPTANCE_AND_CLEANUP_FAILED',
    )
  }
  if (hasPrimaryError) throw primaryError
  if (cleanupErrors.length > 0) {
    throw new AggregateError(cleanupErrors, 'REAL_EXPERIMENT_STUDENT_ACCEPTANCE_CLEANUP_FAILED')
  }
}

test('teacher publishes a real security experiment and student repairs it through the browser terminal', async ({ browser, page, baseURL }, testInfo) => {
  test.skip(!config && !resume, 'Set the real provider settings for a full run or explicit resume project and approval IDs for the student continuation.')
  if (!baseURL || (!config && !resume)) throw new Error('REAL_EXPERIMENT_BASE_URL_AND_RESUME_CONTEXT_REQUIRED')

  const request = page.context().request
  if (resume) {
    const resumed = await readResumablePublishedExperiment(request, resume)
    await continueStudentAcceptance({
      browser,
      teacherPage: page,
      baseURL,
      projectId: resumed.project.id,
      publication: resumed.publication,
      testInfo,
    })
    return
  }

  const packageCopy = await createSecurityControlledPackage(config.goldenBaseImage)
  try {
    const project = await createProjectByUi(page, `real-security-${Date.now()}-${uuidv7().slice(0, 8)}`)
    await selectProjectByUi(page, project.id)
    await createProjectPolicy(request, baseURL, project.id, REAL_PROVIDER_BUDGET)
    await page.goto(`/teacher/materials?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
    await selectProjectByUi(page, project.id)
    await expect(page.getByRole('heading', { name: '材料上传与 AgentRun', exact: true })).toBeVisible()
    const packageData = await uploadPackageDirectoryByUi(page, packageCopy.directory)
    await page.screenshot({ path: testInfo.outputPath('materials.png'), fullPage: true })

    const run = await startExperimentRunByUi(page, project.id)
    await expect(page).toHaveURL(new RegExp(`[?&]runId=${encodeURIComponent(run.id)}(?:&|$)`), { timeout: 30_000 })
    await page.reload({ waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('heading', { name: '生成实验候选', exact: true })).toBeVisible()
    const completed = await waitForExperimentRun(request, project.id, run.id)
    const built = await waitForBuiltCandidate(request, project.id, completed.environmentCandidateId)
    const published = await approveAndPublish(
      page,
      project.id,
      completed.run.id,
      packageData,
      completed.environmentCandidateId,
      completed.evaluationCandidateId,
      built.artifact,
    )
    await page.screenshot({ path: testInfo.outputPath('approval.png'), fullPage: true })
    await continueStudentAcceptance({
      browser,
      teacherPage: page,
      baseURL,
      projectId: project.id,
      publication: published.publication,
      testInfo,
    })
  } finally {
    await packageCopy.cleanup()
  }
})
