import { expect, test } from '@playwright/test'
import {
  createProjectByUi,
  createProjectPolicy,
  csrfHeaders,
  expectJson,
  pollEnvironmentCandidate,
  pollJson,
  selectProjectByUi,
  uploadPackage,
  uuidv7,
} from '../support/live.mjs'

const AUTHORING_CHAIN_TIMEOUT_MS = 1_800_000

test.describe.configure({ timeout: AUTHORING_CHAIN_TIMEOUT_MS })

function diagnosticCodes(run) {
  return run.tracks
    .flatMap((track) => track.attempts ?? [])
    .map((attempt) => attempt.diagnosticCode)
    .filter(Boolean)
    .join(',') || 'no attempt diagnostic'
}

async function startAuthoringRun(request, baseURL, projectId, packageData, policy) {
  const response = await request.post(`/api/v1/projects/${projectId}/agent-runs`, {
    headers: await csrfHeaders(request, baseURL, { 'Idempotency-Key': uuidv7() }),
    data: {
      projectId,
      courseId: null,
      packageId: packageData.id,
      packageRevision: packageData.revision,
      policyId: policy.id,
      policyRevision: policy.revision,
      environmentClass: 'experiment',
    },
  })
  const run = await expectJson(response, 'AUTHORING_RUN_CREATE_FAILED')
  if (run.projectId !== projectId || run.purpose?.kind !== 'authoring' || run.purpose.environmentClass !== 'experiment') {
    throw new Error(`AUTHORING_RUN_CONTRACT_INVALID:${JSON.stringify(run).slice(0, 2000)}`)
  }
  return run
}

async function waitForCompletedAuthoringRun(request, projectId, runId) {
  const run = await pollJson(
    request,
    `/api/v1/projects/${projectId}/agent-runs/${runId}`,
    (value) => ['succeeded', 'partially_succeeded', 'failed', 'cancelled'].includes(value.state),
    'AUTHORING_RUN_STATUS_FAILED',
    300_000,
  )
  if (run.state !== 'succeeded') throw new Error(`AUTHORING_RUN_FAILED:${run.state}:${diagnosticCodes(run)}`)
  const environment = run.tracks.find((track) => track.kind === 'environment')
  const evaluation = run.tracks.find((track) => track.kind === 'evaluation')
  if (!environment?.candidateId || !evaluation?.candidateId) {
    throw new Error(`AUTHORING_RUN_CANDIDATES_MISSING:${diagnosticCodes(run)}`)
  }
  return { run, environmentCandidateId: environment.candidateId, evaluationCandidateId: evaluation.candidateId }
}

async function waitForBuiltEnvironmentCandidate(request, projectId, candidateId) {
  const candidate = await pollEnvironmentCandidate(
    request,
    projectId,
    candidateId,
    (value) => value.build?.state === 'succeeded' || value.build?.state === 'failed' || value.build?.state === 'cancelled',
    'AUTHORING_CANDIDATE_BUILD_FAILED',
    300_000,
  )
  if (candidate.build?.state !== 'succeeded' || !candidate.imageArtifact) {
    throw new Error(`AUTHORING_CANDIDATE_BUILD_FAILED:${candidate.build?.diagnosticCode ?? 'artifact missing'}`)
  }
  return candidate
}

async function openReview(page, projectId, runId, environmentCandidateId, evaluationCandidateId) {
  await page.goto(`/teacher/approvals?projectId=${encodeURIComponent(projectId)}&runId=${encodeURIComponent(runId)}`, {
    waitUntil: 'domcontentloaded',
  })
  await selectProjectByUi(page, projectId)
  await expect(page.getByRole('heading', { name: '实验包批准', exact: true })).toBeVisible()
  await expect(page.locator('select[aria-label="选择项目"]')).toHaveValue(projectId)
  await expect(page.locator('input[aria-label="AgentRun ID"]')).toHaveValue(runId)
  await expect(page.getByText(environmentCandidateId, { exact: true }).first()).toBeVisible({ timeout: 120_000 })
  await expect(page.getByText(evaluationCandidateId, { exact: true }).first()).toBeVisible({ timeout: 120_000 })
  await expect(page.locator('.candidate-card')).toHaveCount(2)
  await expect(page.locator('.approval-card')).toBeVisible()
}

test('teacher authors an independent project and publishes its complete experiment package', async ({ page, baseURL }) => {
  test.setTimeout(AUTHORING_CHAIN_TIMEOUT_MS)
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')

  const project = await createProjectByUi(page, `live-authoring-${Date.now()}-${uuidv7().slice(0, 8)}`)
  await selectProjectByUi(page, project.id)
  const policy = await createProjectPolicy(page.context().request, baseURL, project.id)
  const packageData = await uploadPackage(page.context().request, baseURL, project.id, policy.revision)
  const initialRun = await startAuthoringRun(page.context().request, baseURL, project.id, packageData, policy)
  const completed = await waitForCompletedAuthoringRun(page.context().request, project.id, initialRun.id)
  await waitForBuiltEnvironmentCandidate(page.context().request, project.id, completed.environmentCandidateId)

  await openReview(page, project.id, completed.run.id, completed.environmentCandidateId, completed.evaluationCandidateId)
  const approveButton = page.getByRole('button', { name: '批准完整实验包', exact: true })
  await expect(approveButton).toBeDisabled()
  await expect(page.getByRole('checkbox')).toHaveCount(1)
  await page.getByRole('checkbox').check()
  const reason = '已核对独立项目材料、Environment 和 Evaluation 候选及运行时镜像身份。'
  await page.locator('textarea.reason-input').fill(reason)
  await expect(approveButton).toBeEnabled()

  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/projects/${project.id}/authoring-approvals`
  })
  await approveButton.click()
  const approvalResponse = await responsePromise
  if (!approvalResponse.ok()) throw new Error(`AUTHORING_APPROVAL_FAILED:${approvalResponse.status()}`)
  const approvalRequest = approvalResponse.request().postDataJSON()
  expect(approvalRequest).toMatchObject({ projectId: project.id, reason })
  const approval = await approvalResponse.json()
  expect(approval).toMatchObject({ projectId: project.id, id: expect.any(String) })

  await expect(page).toHaveURL(new RegExp(`[?&]approvalId=${encodeURIComponent(approval.id)}(?:&|$)`))
  const publication = await pollJson(
    page.context().request,
    `/api/v1/projects/${project.id}/authoring-approvals/${approval.id}`,
    (value) => value.status === 'ready' || value.status === 'failed',
    'AUTHORING_PUBLICATION_STATUS_FAILED',
    300_000,
  )
  if (publication.status === 'failed') {
    throw new Error(`AUTHORING_PUBLICATION_FAILED:${publication.diagnosticCode ?? 'diagnostic missing'}`)
  }
  expect(publication.status).toBe('ready')
  await expect(page.locator('.publication-status')).toHaveAttribute('data-status', 'ready', { timeout: 120_000 })

  await page.reload({ waitUntil: 'domcontentloaded' })
  await expect(page).toHaveURL(new RegExp(`[?&]projectId=${encodeURIComponent(project.id)}(?:&|$)`))
  await expect(page).toHaveURL(new RegExp(`[?&]runId=${encodeURIComponent(completed.run.id)}(?:&|$)`))
  await expect(page).toHaveURL(new RegExp(`[?&]approvalId=${encodeURIComponent(approval.id)}(?:&|$)`))
  await expect(page.locator('.publication-status')).toHaveAttribute('data-status', 'ready', { timeout: 120_000 })
  await expect(page.getByRole('button', { name: '批准完整实验包', exact: true })).toHaveCount(0)
})
