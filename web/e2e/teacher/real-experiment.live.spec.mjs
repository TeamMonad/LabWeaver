import { expect, test } from '@playwright/test'
import {
  AUTH_STATE,
  createProjectByUi,
  configureProjectPolicyByUi,
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
  waitForAuthoringRunWithResourceApproval,
  startExperimentRunByUi,
  uploadPackageDirectoryByUi,
  waitForEnvironment,
} from '../support/real-experiment.mjs'
import { deleteEnvironmentByUi } from '../support/environment-lifecycle.mjs'
import { revokeEnvironmentAccessGrantByUi } from '../support/ssh-access.mjs'
import {
  approveAndPublishByUi,
  expectPublicationStatusByUi,
  expectReviewTechnicalIdentitiesByUi,
  openAuthoringReviewByUi,
} from '../support/authoring-review.mjs'

const REAL_CHAIN_TIMEOUT_MS = 3_600_000
// The real chain must prove a single uncontested provider attempt, so it opts out
// of the shared harness retry. Everything else comes from the acceptance budget.
const REAL_PROVIDER_BUDGET = Object.freeze({ maxTransientRetries: 0 })
const config = realProviderConfig()
const resume = realExperimentResumeConfig()

test.describe.configure({ timeout: REAL_CHAIN_TIMEOUT_MS, retries: 0 })

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

async function waitForExperimentRun(request, projectId, runId, adminPage, requesterId) {
  const run = await waitForAuthoringRunWithResourceApproval({
    request,
    adminPage,
    projectId,
    runId,
    requesterId,
    validateRun: (value) => {
      if (value.purpose?.kind !== 'authoring' || value.purpose.environmentClass !== 'experiment') {
        throw new Error('REAL_EXPERIMENT_AGENT_RUN_PURPOSE_INVALID')
      }
    },
  })
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
  await openAuthoringReviewByUi(page, projectId, runId)
  await expectReviewTechnicalIdentitiesByUi(page, [environmentCandidateId, evaluationCandidateId])
  const approval = await approveAndPublishByUi(
    page,
    projectId,
    '已核对真实生成的环境、评分规则、私有黄金基础镜像来源和构建产物摘要。',
    { label: 'REAL_EXPERIMENT_APPROVAL_FAILED' },
  )
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
  await expectPublicationStatusByUi(page, 'ready')
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
    (value) => Array.isArray(value.items) && value.items.filter((item) => item.state === 'active').length === 1,
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
  const consolePanel = page.locator('.console-panel')
  await expect(consolePanel).toBeVisible({ timeout: 120_000 })
  const openTerminal = consolePanel.getByRole('button', { name: '打开终端', exact: true })
  await expect(openTerminal).toBeEnabled({ timeout: 120_000 })
  await openTerminal.click()
  const host = page.locator('.xterm-host')
  await expect(host).toBeVisible({ timeout: 120_000 })
  const input = page.locator('.xterm-helper-textarea')
  await expect(input).toBeAttached({ timeout: 30_000 })
  return { input, terminalFrames }
}

async function editThroughTerminal({ page, input, terminalFrames }) {
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
  await host.click()
  await expect(input).toBeAttached({ timeout: 30_000 })
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

async function revokeEnvironmentAccessGrants(page, projectId, environmentId) {
  const listResponse = await page.request.get(`/api/v1/environments/${environmentId}/access-grants?includeTerminal=false&limit=100`)
  if (listResponse.status() === 404) return
  const listed = await expectJson(listResponse, 'REAL_EXPERIMENT_ACCESS_GRANTS_CLEANUP_LIST_FAILED')
  if (!Array.isArray(listed.items)) throw new Error('REAL_EXPERIMENT_ACCESS_GRANTS_CLEANUP_LIST_INVALID')
  for (const item of listed.items) {
    let grantResponse = await page.request.get(`/api/v1/access-grants/${item.id}`)
    if (grantResponse.status() === 404) continue
    let grant = await expectJson(grantResponse, 'REAL_EXPERIMENT_ACCESS_GRANT_CLEANUP_READ_FAILED')
    if (!['requested', 'active'].includes(grant.state)) continue
    if (grant.state === 'requested') throw new Error(`REAL_EXPERIMENT_ACCESS_GRANT_REQUESTED_NOT_VISIBLE:${grant.id}`)
    await revokeEnvironmentAccessGrantByUi(page, projectId, environmentId, grant.id)
    grant = await pollJson(
      page.request,
      `/api/v1/access-grants/${grant.id}`,
      (value) => ['revoked', 'denied', 'expired'].includes(value.state),
      'REAL_EXPERIMENT_ACCESS_GRANT_CLEANUP_STATUS_FAILED',
      120_000,
    )
    if (grant.state !== 'revoked') throw new Error(`REAL_EXPERIMENT_ACCESS_GRANT_NOT_REVOKED:${grant.state}`)
  }
}

async function closeExperimentEnvironment(page, projectId, environmentId) {
  await revokeEnvironmentAccessGrants(page, projectId, environmentId)
  await deleteEnvironmentByUi(page, {
    routePrefix: 'student',
    projectId,
    environmentId,
    label: 'REAL_EXPERIMENT_ENVIRONMENT_DELETE',
  })
}

const TASK_STAGES = ['queued', 'approval', 'running', 'result']

/**
 * While the submission has no result yet, the environment page links to it
 * and a second tab ("leave and come back") shows the server stage of the same
 * submission without a percentage. The environment page stays open so the
 * freeze flow continues untouched.
 */
async function expectSubmissionProgressShown(page, projectId, submissionId) {
  const tracking = page.locator('[data-testid="submission-tracking"]')
  await expect(tracking).toBeVisible({ timeout: 120_000 })
  await expect(tracking.locator('[aria-current="step"]')).toHaveCount(1, { timeout: 120_000 })
  const href = await tracking.getByRole('link').getAttribute('href')
  if (!href) throw new Error('REAL_EXPERIMENT_SUBMISSION_TRACKING_LINK_MISSING')
  const target = new URL(href, page.url())
  expect(target.searchParams.get('projectId')).toBe(projectId)
  expect(target.searchParams.get('submissionId')).toBe(submissionId)

  const progressPath = `/api/v1/projects/${projectId}/me/evaluation-tasks/submissions/${submissionId}`
  const second = await page.context().newPage()
  try {
    const progressRead = second.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'GET' && url.pathname === progressPath
    }, { timeout: 120_000 })
    await second.goto(`${target.pathname}${target.search}`, { waitUntil: 'domcontentloaded' })
    const response = await progressRead
    if (![200, 404].includes(response.status())) {
      throw new Error(`REAL_EXPERIMENT_EVALUATION_TASK_PROGRESS_STATUS:${response.status()}`)
    }
    if (target.pathname.startsWith('/student/results/')) {
      // The link names a run only after the server mapped the submission to it.
      const body = await response.json()
      expect(target.pathname).toBe(`/student/results/${body.runId}`)
      await expect(second.locator('[data-testid="evaluation-in-progress"], [data-testid="evaluation-outcome"]'))
        .toHaveCount(1, { timeout: 120_000 })
    } else {
      const card = second.locator(`[data-testid="pending-submissions"] [data-submission-id="${submissionId}"]`)
      // The result may already have arrived; otherwise the waiting card shows a server stage.
      if (await card.count() > 0) {
        await expect(card).toHaveClass(/result-card--highlight/)
        expect(TASK_STAGES).toContain(await card.locator('[aria-current="step"]').getAttribute('data-stage'))
        await expect(card).not.toContainText('%')
      }
    }
  } finally {
    await second.close()
  }
}

async function continueStudentAcceptance({ browser, teacherPage, baseURL, projectId, publication }) {
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
    await addProjectStudentByUi(teacherPage, projectId)
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

    await revokeEnvironmentAccessGrants(studentContext.request, baseURL, environmentId)
    const terminal = await issueTerminalAccessAndConnect(studentPage, projectId, environmentId)
    await editThroughTerminal({ page: studentPage, ...terminal })
    await assertNoPendingEvaluationTaskResourceRequests(adminPage.request, projectId, studentActorId)
    const afterRequestIds = await snapshotProjectResourceRequestIds(adminPage.request, projectId)
    const after = await freezeStudentSourceByUi(studentPage, projectId, environmentId)
    // Before any approval: the environment page and the results page show the
    // server stage of this submission, not a guess and not a percentage.
    await expectSubmissionProgressShown(studentPage, projectId, after.id)
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
    // The progress read for the submission resolves to the same run that produced the result.
    const afterTask = await expectJson(
      await studentContext.request.get(`/api/v1/projects/${projectId}/me/evaluation-tasks/submissions/${after.id}`),
      'REAL_EXPERIMENT_EVALUATION_TASK_READ_FAILED',
    )
    expect(afterTask).toMatchObject({ runId: afterResult.runId, projectId, state: 'succeeded' })
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
    // The submission has a result, so it is no longer listed as in progress.
    const pendingCard = studentPage.locator(`[data-testid="pending-submissions"] [data-submission-id="${after.id}"]`)
    await expect(pendingCard).toHaveCount(0, { timeout: 120_000 })
    const afterResultCard = studentPage.locator(`.result-card[data-run-id="${afterResult.runId}"]`)
    await expect(afterResultCard).toHaveCount(1, { timeout: 120_000 })
    await expect(afterResultCard.locator('.outcome-chip')).toHaveText('评测完成')
    await expect(afterResultCard.locator('.result-score')).toHaveText(`${afterResult.awardedScore} / ${afterResult.maxScore}`)
    await expect(afterResultCard.locator('.result-card__meta')).toContainText('完成于')
    const resultLink = afterResultCard.locator('.result-link')
    await expect(resultLink).not.toHaveText(afterResult.runId)
    await resultLink.click()
    await expect(studentPage).toHaveURL(
      (url) => url.pathname === `/student/results/${afterResult.runId}`
        && url.searchParams.get('projectId') === projectId,
    )
    await expect(studentPage.locator('.eyebrow')).toHaveText('评测详情')
    await expect(studentPage.locator('[data-testid="evaluation-outcome"] .outcome-card__score')).toHaveText(
      `${afterResult.awardedScore} / ${afterResult.maxScore}`,
    )
    await expect(studentPage.locator('.goal-review')).toBeVisible({ timeout: 120_000 })
    await studentPage.locator('a.back-link').click()
    await expect(studentPage).toHaveURL(
      (url) => url.pathname === '/student/results' && url.searchParams.get('projectId') === projectId,
    )
  } catch (error) {
    hasPrimaryError = true
    primaryError = error
  }

  const cleanupErrors = []
  try {
    if (studentPage && environmentId) await closeExperimentEnvironment(studentPage, projectId, environmentId)
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

test('teacher publishes a real security experiment and student repairs it through the browser terminal', async ({ browser, page, baseURL }) => {
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
    })
    return
  }

  const packageCopy = await createSecurityControlledPackage(config.goldenBaseImage)
  const teacherActorId = await readActorId(request)
  const adminContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  const adminPage = await adminContext.newPage()
  try {
    const project = await createProjectByUi(page, `real-security-${Date.now()}-${uuidv7().slice(0, 8)}`)
    await selectProjectByUi(page, project.id)
    await configureProjectPolicyByUi(page, project.id, REAL_PROVIDER_BUDGET)
    await page.goto(`/teacher/materials?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
    await selectProjectByUi(page, project.id)
    await expect(page.getByRole('heading', { name: '材料上传与实验生成', exact: true })).toBeVisible()
    const packageData = await uploadPackageDirectoryByUi(page, packageCopy.directory)

    const run = await startExperimentRunByUi(page, project.id)
    await expect(page).toHaveURL(new RegExp(`[?&]runId=${encodeURIComponent(run.id)}(?:&|$)`), { timeout: 30_000 })
    await page.reload({ waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('heading', { name: '生成实验候选', exact: true })).toBeVisible()
    const completed = await waitForExperimentRun(request, project.id, run.id, adminPage, teacherActorId)
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
    await continueStudentAcceptance({
      browser,
      teacherPage: page,
      baseURL,
      projectId: project.id,
      publication: published.publication,
    })
  } finally {
    await adminContext.close()
    await packageCopy.cleanup()
  }
})
