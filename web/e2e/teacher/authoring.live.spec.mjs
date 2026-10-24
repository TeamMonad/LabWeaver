import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { expect, test } from '@playwright/test'
import {
  createProjectByUi,
  configureProjectPolicyByUi,
  pollEnvironmentCandidate,
  pollJson,
  selectProjectByUi,
  uuidv7,
} from '../support/live.mjs'
import { startExperimentRunByUi, uploadPackageDirectoryByUi } from '../support/real-experiment.mjs'
import {
  approveAndPublishByUi,
  expectPublicationStatusByUi,
  expectPublishedReviewByUi,
  expectReviewTechnicalIdentitiesByUi,
  openAuthoringReviewByUi,
  reopenAuthoringReviewByLink,
} from '../support/authoring-review.mjs'

const AUTHORING_CHAIN_TIMEOUT_MS = 1_800_000

test.describe.configure({ timeout: AUTHORING_CHAIN_TIMEOUT_MS })

function diagnosticCodes(run) {
  return run.tracks
    .flatMap((track) => track.attempts ?? [])
    .map((attempt) => attempt.diagnosticCode)
    .filter(Boolean)
    .join(',') || 'no attempt diagnostic'
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

test('teacher authors an independent project and publishes its complete experiment package', async ({ page, baseURL }) => {
  test.setTimeout(AUTHORING_CHAIN_TIMEOUT_MS)
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')

  const project = await createProjectByUi(page, `live-authoring-${Date.now()}-${uuidv7().slice(0, 8)}`)
  await selectProjectByUi(page, project.id)
  await configureProjectPolicyByUi(page, project.id)
  const packageDirectory = await mkdtemp(join(tmpdir(), 'labweaver-authoring-'))
  await writeFile(join(packageDirectory, 'README.md'), '# LabWeaver authoring fixture\n', 'utf8')
  let initialRun
  try {
    await page.goto(`/teacher/materials?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
    await selectProjectByUi(page, project.id)
    const packageData = await uploadPackageDirectoryByUi(page, packageDirectory, 'README.md')
    initialRun = await startExperimentRunByUi(page, project.id, packageData)
  } finally {
    await rm(packageDirectory, { recursive: true, force: true })
  }
  const completed = await waitForCompletedAuthoringRun(page.context().request, project.id, initialRun.id)
  await waitForBuiltEnvironmentCandidate(page.context().request, project.id, completed.environmentCandidateId)

  // The teacher finds the version in the review queue; no internal ID is typed.
  await openAuthoringReviewByUi(page, project.id, completed.run.id)
  await expect(page.getByRole('tab')).toHaveText(['概览', '环境与权限', '评分规则', '材料', '技术详情'])
  await expectReviewTechnicalIdentitiesByUi(page, [completed.run.id, completed.environmentCandidateId, completed.evaluationCandidateId])

  const reason = '已核对独立项目材料、环境与评分规则候选及运行时镜像身份。'
  const approval = await approveAndPublishByUi(page, project.id, reason)
  expect(approval).toMatchObject({
    projectId: project.id,
    environmentCandidateId: completed.environmentCandidateId,
    evaluationCandidateId: completed.evaluationCandidateId,
  })

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
  await expectPublicationStatusByUi(page, 'ready')

  // Refresh keeps the same project, run and approval and does not offer a second approval.
  await page.reload({ waitUntil: 'domcontentloaded' })
  await expect(page).toHaveURL(new RegExp(`[?&]projectId=${encodeURIComponent(project.id)}(?:&|$)`))
  await expect(page).toHaveURL(new RegExp(`[?&]runId=${encodeURIComponent(completed.run.id)}(?:&|$)`))
  await expect(page).toHaveURL(new RegExp(`[?&]approvalId=${encodeURIComponent(approval.id)}(?:&|$)`))
  await expectPublicationStatusByUi(page, 'ready')
  await expect(page.getByTestId('approve-publish')).toHaveCount(0)

  // Opened again without the approval id, the published run sits under “已发布”.
  await expectPublishedReviewByUi(page, project.id, completed.run.id)
  await reopenAuthoringReviewByLink(page, project.id, completed.run.id, approval.id)
  await expectReviewTechnicalIdentitiesByUi(page, [approval.id, publication.environmentReleaseId, publication.evaluationReleaseId])
})
