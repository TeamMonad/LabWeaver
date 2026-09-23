import { expect, test } from '@playwright/test'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  AUTH_STATE,
  createProjectByUi,
  createProjectPolicy,
  expectJson,
  pollEnvironmentCandidate,
  pollJson,
  selectProjectByUi,
  uuidv7,
} from '../support/live.mjs'
import { addProjectStudentByUi, readActorId } from '../support/real-experiment.mjs'
import {
  ADMIN_LEASE_STATE,
  ADMIN_REQUEST_STATE,
  RESEARCHER_LEASE_STATE,
  RESEARCHER_REQUEST_STATE,
  WORK_PROVIDER_BINDING,
  approveResourceRequestByUi,
  assertGpuCatalogByUi,
  assertProjectChargesByUi,
  cancelProjectResourceRequestByUi,
  readBackLeaseByUi,
  releaseProjectLeaseByUi,
  requestProjectResourceByUi,
} from '../support/real-resource.mjs'
import { assertNoStuckProgress, auditAccessibility, installUsabilityGuards } from '../support/usability.mjs'

/**
 * A resource request can only bind an environment template that is already
 * published, so this journey publishes its own Work template through the real
 * authoring UI before the student submits anything.
 */
const WORK_TEMPLATE_PACKAGE_CONTENT = '# LabWeaver live Work fixture\n\nUse the managed environment.\n'
const WORK_TEMPLATE_APPROVAL_REASON = '已核对 Work EnvironmentSpec、容器 artifact 和项目安全约束。'
const JOURNEY_TIMEOUT_MS = 1_800_000
const SETTLE_TIMEOUT_MS = 120_000

function diagnosticCode(value) {
  return value?.diagnosticCode ?? value?.diagnostic_code ?? 'diagnostic missing'
}

function terminalRunState(value) {
  return ['succeeded', 'partially_succeeded', 'failed', 'cancelled'].includes(value)
}

function describeError(error) {
  return error instanceof Error ? error.message : String(error)
}

/**
 * Publish a Work environment template for the project through the authoring
 * page, its AgentRun, the candidate approval, and the release operation.
 */
async function publishWorkTemplateByUi(page, projectId) {
  const packageDirectory = await mkdtemp(join(tmpdir(), 'labweaver-admin-work-'))
  try {
    await writeFile(join(packageDirectory, 'README.md'), WORK_TEMPLATE_PACKAGE_CONTENT, 'utf8')
    await page.goto(`/researcher/software?projectId=${encodeURIComponent(projectId)}`, { waitUntil: 'domcontentloaded' })
    await selectProjectByUi(page, projectId)
    await page.getByRole('button', { name: '生成 Work 模板', exact: true }).click()
    await expect(page.getByRole('heading', { name: '生成 Work 模板', exact: true })).toBeVisible()

    await page.getByTestId('work-template-file-input').setInputFiles(packageDirectory)
    await expect(page.getByRole('list', { name: '待上传材料文件', exact: true })).toContainText('README.md')
    const packageResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && /\/api\/v1\/projects\/[^/]+\/problem-package-uploads\/[^/]+\/complete$/.test(url.pathname)
    })
    await page.getByRole('button', { name: '上传材料包', exact: true }).click()
    const packageData = await expectJson(await packageResponsePromise, 'LW_ACCEPTANCE_WORK_PACKAGE_UPLOAD_FAILED')
    expect(packageData).toMatchObject({ projectId, revision: expect.any(Number) })
    await expect(page.locator('.package-summary').getByText(/材料包已归档：/)).toBeVisible({ timeout: SETTLE_TIMEOUT_MS })

    const runResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${projectId}/agent-runs`
    })
    await page.getByRole('button', { name: '启动 Work AgentRun', exact: true }).click()
    const acceptedRun = await expectJson(await runResponsePromise, 'LW_ACCEPTANCE_WORK_TEMPLATE_RUN_CREATE_FAILED')
    expect(acceptedRun).toMatchObject({ id: expect.any(String), projectId })
    const run = await pollJson(
      page.request,
      `/api/v1/projects/${projectId}/agent-runs/${acceptedRun.id}`,
      (value) => terminalRunState(value.state),
      'LW_ACCEPTANCE_WORK_TEMPLATE_RUN_STATUS_FAILED',
      300_000,
    )
    if (run.state !== 'succeeded') {
      const attempts = run.tracks?.map((track) => track.attempts?.map(diagnosticCode).join(',')).join(';') ?? 'no tracks'
      throw new Error(`LW_ACCEPTANCE_WORK_TEMPLATE_RUN_FAILED:${run.state}:${attempts}`)
    }
    const environmentTrack = run.tracks.find((track) => track.kind === 'environment')
    if (!environmentTrack?.candidateId) throw new Error('LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_MISSING')

    const candidate = await pollEnvironmentCandidate(
      page.request,
      projectId,
      environmentTrack.candidateId,
      (value) => ['succeeded', 'failed', 'cancelled'].includes(value.build?.state),
      'LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_BUILD_STATUS_FAILED',
      300_000,
    )
    if (candidate.candidate?.spec?.class !== 'work') throw new Error('LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_CLASS_INVALID')
    if (candidate.build?.state !== 'succeeded' || !candidate.imageArtifact) {
      throw new Error(`LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_BUILD_FAILED:${candidate.build?.diagnosticCode ?? 'artifact missing'}`)
    }

    const candidateCard = page.getByTestId('work-template-candidate')
    await expect(candidateCard).toBeVisible({ timeout: SETTLE_TIMEOUT_MS })
    await expect(candidateCard).toContainText('构建完成', { timeout: SETTLE_TIMEOUT_MS })
    await candidateCard.getByTestId('work-template-candidate-confirmation').check()
    await candidateCard.getByPlaceholder('说明为什么批准这个 Work Environment 候选').fill(WORK_TEMPLATE_APPROVAL_REASON)
    const approveCandidateButton = candidateCard.getByRole('button', { name: '批准 Environment 候选', exact: true })
    await expect(approveCandidateButton).toBeEnabled()
    const approvalResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${projectId}/environment-candidates/${environmentTrack.candidateId}/decisions`
    })
    await approveCandidateButton.click()
    const approval = await expectJson(await approvalResponsePromise, 'LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_APPROVAL_FAILED')
    expect(approval).toMatchObject({ candidateId: environmentTrack.candidateId, decision: 'approved' })
    await expect(candidateCard).toContainText(`候选已批准：${approval.id}`)

    const releaseResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${projectId}/environment-template-releases`
    })
    await page.getByTestId('work-template-release-button').click()
    const releaseAccepted = await expectJson(await releaseResponsePromise, 'LW_ACCEPTANCE_WORK_TEMPLATE_RELEASE_CREATE_FAILED')
    expect(releaseAccepted).toMatchObject({ operationId: expect.any(String), statusUrl: expect.any(String) })
    const release = await pollJson(
      page.request,
      releaseAccepted.statusUrl,
      (value) => Boolean(value.id) && value.projectId === projectId && value.candidateId === environmentTrack.candidateId,
      'LW_ACCEPTANCE_WORK_TEMPLATE_RELEASE_STATUS_FAILED',
      SETTLE_TIMEOUT_MS,
    )
    expect(release).toMatchObject({ projectId, runtimeKind: 'container', version: expect.any(Number) })
    await expect(page.getByTestId('work-template-resource-link')).toBeVisible()
    return { packageData, run, release }
  } finally {
    await rm(packageDirectory, { recursive: true, force: true })
  }
}

test('platform administrator approves a real resource request and reads back its lease and charges', async ({ browser, page, baseURL }, testInfo) => {
  test.setTimeout(JOURNEY_TIMEOUT_MS)
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')

  const adminGuards = installUsabilityGuards(page)
  let teacherContext = null
  let studentContext = null
  let studentPage = null
  let project = null
  let resourceRequest = null
  let approval = null
  let primaryError = null
  let cleanupError = null
  try {
    teacherContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.teacher })
    const teacherPage = await teacherContext.newPage()
    studentContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.student })
    studentPage = await studentContext.newPage()
    const studentGuards = installUsabilityGuards(studentPage)

    project = await createProjectByUi(teacherPage, `live-admin-${Date.now()}-${uuidv7().slice(0, 8)}`)
    await selectProjectByUi(teacherPage, project.id)
    await createProjectPolicy(teacherPage.request, baseURL, project.id)
    const studentActorId = await readActorId(studentContext.request)
    await addProjectStudentByUi(teacherPage, project.id, studentActorId)
    await publishWorkTemplateByUi(teacherPage, project.id)
    await teacherPage.screenshot({ path: testInfo.outputPath('work-template-published.png'), fullPage: true })

    resourceRequest = await requestProjectResourceByUi(studentPage, {
      projectName: project.name,
      projectId: project.id,
      kind: 'cpu',
    })
    expect(resourceRequest.state).toBe(RESEARCHER_REQUEST_STATE.reviewing)
    await assertNoStuckProgress(studentPage, 'researcher-resources')
    await auditAccessibility(studentPage, 'researcher-resources', testInfo)
    studentGuards.assertCleanConsole('researcher-resources')
    await studentPage.screenshot({ path: testInfo.outputPath('resource-request.png'), fullPage: true })

    approval = await approveResourceRequestByUi(page, {
      projectName: project.name,
      projectId: project.id,
      requestKey: resourceRequest.requestKey,
      requestId: resourceRequest.requestId,
      environmentId: resourceRequest.environmentId,
      requesterId: studentActorId,
      durationSeconds: resourceRequest.durationSeconds,
      providerBinding: WORK_PROVIDER_BINDING,
    })
    expect(approval.requestState).toBe(ADMIN_REQUEST_STATE.active)
    expect(approval.leaseState).toBe(ADMIN_LEASE_STATE.active)
    expect(approval.expiresAtLabel, 'LW_ACCEPTANCE_ADMIN_LEASE_EXPIRY_MISSING').not.toBe('—')
    expect(Date.parse(approval.expiresAt), 'LW_ACCEPTANCE_ADMIN_LEASE_EXPIRY_NOT_FUTURE').toBeGreaterThan(Date.now())
    await assertNoStuckProgress(page, 'admin-resource-approval')
    await auditAccessibility(page, 'admin-resource-approval', testInfo)
    adminGuards.assertCleanConsole('admin-resource-approval')
    await page.screenshot({ path: testInfo.outputPath('resource-approval.png'), fullPage: true })

    const lease = await readBackLeaseByUi(studentPage, {
      projectName: project.name,
      projectId: project.id,
      leaseId: approval.leaseId,
    })
    expect(lease.leaseId).toBe(approval.leaseId)
    expect(lease.state).toBe(RESEARCHER_LEASE_STATE.active)
    expect(lease.expiresAtLabel, 'LW_ACCEPTANCE_LEASE_EXPIRY_MISSING').toContain('到期')
    expect(Date.parse(lease.expiresAt), 'LW_ACCEPTANCE_LEASE_EXPIRY_NOT_FUTURE').toBeGreaterThan(Date.now())
    await studentPage.screenshot({ path: testInfo.outputPath('lease-readback.png'), fullPage: true })

    const finance = await assertProjectChargesByUi(page, project)
    await assertNoStuckProgress(page, 'admin-resource-finance')
    await auditAccessibility(page, 'admin-resource-finance', testInfo)
    adminGuards.assertCleanConsole('admin-resource-finance')
    await page.screenshot({ path: testInfo.outputPath('resource-finance.png'), fullPage: true })

    const catalog = await assertGpuCatalogByUi(page)
    await assertNoStuckProgress(page, 'admin-gpu-catalog')
    await auditAccessibility(page, 'admin-gpu-catalog', testInfo)
    adminGuards.assertCleanConsole('admin-gpu-catalog')
    await page.screenshot({ path: testInfo.outputPath('gpu-catalog.png'), fullPage: true })

    testInfo.annotations.push({
      type: 'acceptance',
      description: JSON.stringify({
        projectId: project.id,
        requestId: resourceRequest.requestId,
        requestKey: resourceRequest.requestKey,
        environmentId: resourceRequest.environmentId,
        leaseId: approval.leaseId,
        leaseExpiresAt: approval.expiresAt,
        finance: finance.kind,
        gpuCatalog: catalog.kind,
      }),
    })
  } catch (error) {
    primaryError = error
  } finally {
    try {
      if (studentPage && project) {
        if (approval?.leaseId) {
          await releaseProjectLeaseByUi(studentPage, {
            projectName: project.name,
            projectId: project.id,
            leaseId: approval.leaseId,
          })
        } else if (resourceRequest?.requestKey) {
          await cancelProjectResourceRequestByUi(studentPage, {
            projectName: project.name,
            projectId: project.id,
            requestKey: resourceRequest.requestKey,
          })
        }
      }
    } catch (error) {
      cleanupError = error
    } finally {
      await studentContext?.close()
      await teacherContext?.close()
    }
  }
  if (primaryError) {
    throw cleanupError
      ? new Error(
        `LW_ACCEPTANCE_PRIMARY_FAILURE:${describeError(primaryError)};LW_ACCEPTANCE_CLEANUP_FAILED:${describeError(cleanupError)}`,
        { cause: primaryError },
      )
      : primaryError
  }
  if (cleanupError) throw cleanupError
})
