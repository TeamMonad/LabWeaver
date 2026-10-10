import { expect, test } from '@playwright/test'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
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
const WORK_TEMPLATE_APPROVAL_REASON = '已核对 Work 环境候选规格、容器 artifact 和项目安全约束。'
const GPU_MODE = process.env.LABWEAVER_E2E_GPU_MODE?.trim() || null
const GPU_CLASS = process.env.LABWEAVER_E2E_GPU_CLASS?.trim() || null
const AUTHORING_RESOURCE_PROVIDER_BINDING =
  process.env.LABWEAVER_E2E_AUTHORING_PROVIDER_BINDING?.trim()
  || process.env.LABWEAVER_E2E_PROVIDER_BINDING?.trim()
  || 'container-primary-v1'
// The agent worker runs one reserved dispatch at a time, so the Work template
// authoring this journey drives can sit behind earlier runs; these ceilings cover
// a queued authoring run plus the deployment's own fifteen minute per-candidate
// LLM bound and the image build that follows it.
const JOURNEY_TIMEOUT_MS = 7_200_000
const SETTLE_TIMEOUT_MS = 1_800_000
const AUTHORING_RUN_TIMEOUT_MS = 2_700_000
const CANDIDATE_BUILD_TIMEOUT_MS = 1_800_000
const ACTIVE_ATTEMPT_STATES = new Set(['pending', 'running', 'repairing', 'awaiting_approval'])

function diagnosticCode(value) {
  return value?.diagnosticCode ?? value?.diagnostic_code ?? 'diagnostic missing'
}

function terminalRunState(value) {
  return ['succeeded', 'partially_succeeded', 'failed', 'cancelled'].includes(value)
}

function runIsFullyTerminal(run) {
  return terminalRunState(run.state)
    && !run.tracks.some((track) => track.attempts.some((attempt) => ACTIVE_ATTEMPT_STATES.has(attempt.state)))
}

function describeError(error) {
  return error instanceof Error ? error.message : String(error)
}

/**
 * Publish a Work environment template for the project through the authoring
 * page, its AgentRun, the candidate approval, and the release operation.
 */
async function publishWorkTemplateByUi(teacherPage, adminPage, projectId, teacherActorId) {
  const packageDirectory = await mkdtemp(join(tmpdir(), 'labweaver-admin-work-'))
  try {
    await writeFile(join(packageDirectory, 'README.md'), WORK_TEMPLATE_PACKAGE_CONTENT, 'utf8')
    await teacherPage.goto(`/researcher/software?projectId=${encodeURIComponent(projectId)}`, { waitUntil: 'domcontentloaded' })
    await selectProjectByUi(teacherPage, projectId)
    await teacherPage.getByRole('button', { name: '生成 Work 模板', exact: true }).click()
    await expect(teacherPage.getByRole('heading', { name: '生成 Work 模板', exact: true })).toBeVisible()

    await teacherPage.getByTestId('work-template-file-input').setInputFiles(packageDirectory)
    await expect(teacherPage.getByRole('list', { name: '待上传材料文件', exact: true })).toContainText('README.md')
    const packageResponsePromise = teacherPage.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && /\/api\/v1\/projects\/[^/]+\/problem-package-uploads\/[^/]+\/complete$/.test(url.pathname)
    })
    await teacherPage.getByRole('button', { name: '上传材料包', exact: true }).click()
    const packageData = await expectJson(await packageResponsePromise, 'LW_ACCEPTANCE_WORK_PACKAGE_UPLOAD_FAILED')
    expect(packageData).toMatchObject({ projectId, revision: expect.any(Number) })
    await expect(teacherPage.locator('.package-summary').getByText(/材料包已归档：/)).toBeVisible({ timeout: SETTLE_TIMEOUT_MS })

    const runResponsePromise = teacherPage.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${projectId}/agent-runs`
    })
    await teacherPage.getByRole('button', { name: '启动 Work 模板生成', exact: true }).click()
    const acceptedRun = await expectJson(await runResponsePromise, 'LW_ACCEPTANCE_WORK_TEMPLATE_RUN_CREATE_FAILED')
    expect(acceptedRun).toMatchObject({
      id: expect.any(String),
      projectId,
      packageId: packageData.id,
      purpose: { kind: 'authoring', environmentClass: 'work' },
    })

    const compactRunId = acceptedRun.id.replaceAll('-', '').toLowerCase()
    const approvePendingAuthoringResources = async () => {
      const requests = await expectJson(
        await adminPage.request.get(`/api/v1/projects/${projectId}/resource-requests`),
        'LW_ACCEPTANCE_WORK_TEMPLATE_RESOURCE_REQUESTS_READ_FAILED',
      )
      if (!Array.isArray(requests)) throw new Error('LW_ACCEPTANCE_WORK_TEMPLATE_RESOURCE_REQUESTS_INVALID')
      const runPrefix = `authoring-${compactRunId}-`
      for (const resourceRequest of requests.filter((item) => (
        item?.projectId === projectId
        && typeof item.requestKey === 'string'
        && item.requestKey.startsWith(runPrefix)
      ))) {
        const requestIdentity = resourceRequest.requestKey.match(
          /^authoring-([0-9a-f]{32})-(environment|evaluation|work_configuration)-([1-9][0-9]*)-([0-9a-f]{32})$/i,
        )
        const taskRunId = resourceRequest.target?.taskRunId
        if (
          requestIdentity?.[1] !== compactRunId
          || resourceRequest.requesterId !== teacherActorId
          || resourceRequest.target?.kind !== 'task'
          || typeof taskRunId !== 'string'
          || taskRunId.replaceAll('-', '').toLowerCase() !== requestIdentity?.[4]?.toLowerCase()
        ) {
          throw new Error(`LW_ACCEPTANCE_WORK_TEMPLATE_RESOURCE_REQUEST_SCOPE_INVALID:${resourceRequest.id ?? 'missing'}`)
        }
        if (resourceRequest.state !== 'reviewing') continue
        if (!Number.isInteger(resourceRequest.requestedDurationSeconds) || resourceRequest.requestedDurationSeconds <= 0) {
          throw new Error(`LW_ACCEPTANCE_WORK_TEMPLATE_RESOURCE_DURATION_INVALID:${resourceRequest.id ?? 'missing'}`)
        }
        await approveResourceRequestByUi(adminPage, {
          requestKey: resourceRequest.requestKey,
          projectId,
          requestId: resourceRequest.id,
          requesterId: teacherActorId,
          durationSeconds: resourceRequest.requestedDurationSeconds,
          providerBinding: AUTHORING_RESOURCE_PROVIDER_BINDING,
        })
      }
    }

    const waitForRun = async (minimumRevision = 0, requiredEnvironmentAttemptNumber = null) => await pollJson(
      teacherPage.request,
      `/api/v1/projects/${projectId}/agent-runs/${acceptedRun.id}`,
      async (value) => {
        if (
          value.id !== acceptedRun.id
          || value.projectId !== projectId
          || value.packageId !== packageData.id
          || value.purpose?.kind !== 'authoring'
          || value.purpose.environmentClass !== 'work'
        ) {
          throw new Error('LW_ACCEPTANCE_WORK_TEMPLATE_RUN_SCOPE_INVALID')
        }
        const revisionIsFresh = Number.isInteger(value.revision) && value.revision >= minimumRevision
        const requiredAttempt = requiredEnvironmentAttemptNumber === null
          ? null
          : value.tracks.find((track) => track.kind === 'environment')?.attempts
            .find((attempt) => attempt.number === requiredEnvironmentAttemptNumber)
        const requiredAttemptIsTerminal = requiredEnvironmentAttemptNumber === null
          || ['succeeded', 'failed', 'cancelled'].includes(requiredAttempt?.state)
        const complete = runIsFullyTerminal(value) && revisionIsFresh && requiredAttemptIsTerminal
        if (!complete) await approvePendingAuthoringResources()
        return complete
      },
      'LW_ACCEPTANCE_WORK_TEMPLATE_RUN_STATUS_FAILED',
      AUTHORING_RUN_TIMEOUT_MS,
    )

    let run = await waitForRun()
    if (run.state !== 'succeeded') {
      const environmentTrack = run.tracks.find((track) => track.kind === 'environment')
      const attempts = environmentTrack?.attempts ?? []
      const firstAttempt = attempts[0]
      const retryableFailure = run.state !== 'cancelled'
        && attempts.length === 1
        && firstAttempt.number === 1
        && ['failed', 'cancelled'].includes(firstAttempt.state)
        && firstAttempt.diagnosticCode?.includes('LW_PROVIDER_UNAVAILABLE')
      if (!retryableFailure) {
        const runDiagnostics = run.tracks.map((track) => track.attempts.map(diagnosticCode).join(',')).join(';') || 'no tracks'
        throw new Error(`LW_ACCEPTANCE_WORK_TEMPLATE_RUN_FAILED:${run.state}:${runDiagnostics}`)
      }

      const retryPage = await teacherPage.context().newPage()
      let acceptedRetry
      try {
        await retryPage.goto(
          `/teacher/materials?projectId=${encodeURIComponent(projectId)}&packageId=${encodeURIComponent(run.packageId)}&runId=${encodeURIComponent(run.id)}`,
          { waitUntil: 'domcontentloaded' },
        )
        await selectProjectByUi(retryPage, projectId)
        const retry = retryPage.getByRole('button', { name: '重试环境轨道', exact: true })
        await expect(retry).toBeVisible({ timeout: SETTLE_TIMEOUT_MS })
        await expect(retry).toBeEnabled()
        const retryResponsePromise = retryPage.waitForResponse((response) => {
          const url = new URL(response.url())
          return response.request().method() === 'POST'
            && url.pathname === `/api/v1/projects/${projectId}/agent-runs/${run.id}/tracks/environment/retry`
        })
        await retry.click()
        const retryResponse = await retryResponsePromise
        const retryHeaders = retryResponse.request().headers()
        expect(retryHeaders['idempotency-key']).toMatch(/^[0-9a-f-]{36}$/i)
        expect(retryHeaders['if-match']).toBe(`"rev-${run.revision}"`)
        acceptedRetry = await expectJson(retryResponse, 'LW_ACCEPTANCE_WORK_TEMPLATE_TRACK_RETRY_FAILED')
      } finally {
        await retryPage.close()
      }
      expect(acceptedRetry).toMatchObject({ id: run.id, projectId })
      expect(acceptedRetry.revision).toBeGreaterThan(run.revision)
      run = await waitForRun(acceptedRetry.revision, 2)
      if (run.state !== 'succeeded') {
        const runDiagnostics = run.tracks.map((track) => track.attempts.map(diagnosticCode).join(',')).join(';') || 'no tracks'
        throw new Error(`LW_ACCEPTANCE_WORK_TEMPLATE_RUN_FAILED_AFTER_TRACK_RETRY:${run.state}:${runDiagnostics}`)
      }
    }

    const environmentTrack = run.tracks.find((track) => track.kind === 'environment')
    if (!environmentTrack?.candidateId) throw new Error('LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_MISSING')
    const candidate = await pollEnvironmentCandidate(
      teacherPage.request,
      projectId,
      environmentTrack.candidateId,
      async (value) => {
        if (
          value.candidate?.id !== environmentTrack.candidateId
          || value.candidate.projectId !== projectId
          || value.candidate.runId !== run.id
        ) {
          throw new Error('LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_SCOPE_INVALID')
        }
        if (!['succeeded', 'failed', 'cancelled'].includes(value.build?.state)) {
          await approvePendingAuthoringResources()
        }
        return ['succeeded', 'failed', 'cancelled'].includes(value.build?.state)
      },
      'LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_BUILD_STATUS_FAILED',
      CANDIDATE_BUILD_TIMEOUT_MS,
    )
    if (candidate.candidate?.spec?.class !== 'work') throw new Error('LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_CLASS_INVALID')
    if (candidate.build?.state !== 'succeeded' || !candidate.imageArtifact) {
      throw new Error(`LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_BUILD_FAILED:${candidate.build?.diagnosticCode ?? 'artifact missing'}`)
    }

    const candidateCard = teacherPage.getByTestId('work-template-candidate')
    await expect(candidateCard).toBeVisible({ timeout: SETTLE_TIMEOUT_MS })
    await expect(candidateCard).toContainText('构建完成', { timeout: SETTLE_TIMEOUT_MS })
    await candidateCard.getByTestId('work-template-candidate-confirmation').check()
    await candidateCard.getByPlaceholder('说明为什么批准这个 Work 环境候选').fill(WORK_TEMPLATE_APPROVAL_REASON)
    const approveCandidateButton = candidateCard.getByRole('button', { name: '批准环境候选', exact: true })
    await expect(approveCandidateButton).toBeEnabled()
    const approvalResponsePromise = teacherPage.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${projectId}/environment-candidates/${environmentTrack.candidateId}/decisions`
    })
    await approveCandidateButton.click()
    const approval = await expectJson(await approvalResponsePromise, 'LW_ACCEPTANCE_WORK_TEMPLATE_CANDIDATE_APPROVAL_FAILED')
    expect(approval).toMatchObject({ candidateId: environmentTrack.candidateId, decision: 'approved' })
    await expect(candidateCard).toContainText(`候选已批准：${approval.id}`)

    const releaseResponsePromise = teacherPage.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${projectId}/environment-template-releases`
    })
    await teacherPage.getByTestId('work-template-release-button').click()
    const releaseAccepted = await expectJson(await releaseResponsePromise, 'LW_ACCEPTANCE_WORK_TEMPLATE_RELEASE_CREATE_FAILED')
    expect(releaseAccepted).toMatchObject({ operationId: expect.any(String), statusUrl: expect.any(String) })
    const release = await pollJson(
      teacherPage.request,
      releaseAccepted.statusUrl,
      (value) => Boolean(value.id) && value.projectId === projectId && value.candidateId === environmentTrack.candidateId,
      'LW_ACCEPTANCE_WORK_TEMPLATE_RELEASE_STATUS_FAILED',
      SETTLE_TIMEOUT_MS,
    )
    expect(release).toMatchObject({ projectId, runtimeKind: 'container', version: expect.any(Number) })
    await expect(teacherPage.getByTestId('work-template-resource-link')).toBeVisible()
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
    await configureProjectPolicyByUi(teacherPage, project.id)
    const teacherActorId = await readActorId(teacherPage.request)
    const studentActorId = await readActorId(studentContext.request)
    await addProjectStudentByUi(teacherPage, project.id)
    await publishWorkTemplateByUi(teacherPage, page, project.id, teacherActorId)

    resourceRequest = await requestProjectResourceByUi(studentPage, {
      projectName: project.name,
      projectId: project.id,
      kind: GPU_MODE ? 'gpu' : 'cpu',
      gpuMode: GPU_MODE,
      gpuClass: GPU_CLASS,
    })
    expect(resourceRequest.state).toBe(RESEARCHER_REQUEST_STATE.reviewing)
    await assertNoStuckProgress(studentPage, 'researcher-resources')
    await auditAccessibility(studentPage, 'researcher-resources', testInfo)
    studentGuards.assertCleanConsole('researcher-resources')

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

    const lease = await readBackLeaseByUi(studentPage, {
      projectName: project.name,
      projectId: project.id,
      leaseId: approval.leaseId,
    })
    expect(lease.leaseId).toBe(approval.leaseId)
    expect(lease.state).toBe(RESEARCHER_LEASE_STATE.active)
    expect(lease.expiresAtLabel, 'LW_ACCEPTANCE_LEASE_EXPIRY_MISSING').toContain('到期')
    expect(Date.parse(lease.expiresAt), 'LW_ACCEPTANCE_LEASE_EXPIRY_NOT_FUTURE').toBeGreaterThan(Date.now())

    const finance = await assertProjectChargesByUi(page, project)
    await assertNoStuckProgress(page, 'admin-resource-finance')
    await auditAccessibility(page, 'admin-resource-finance', testInfo)
    adminGuards.assertCleanConsole('admin-resource-finance')

    const catalog = await assertGpuCatalogByUi(page, {
      requiredModes: GPU_MODE ? [GPU_MODE] : [],
      requiredClass: GPU_CLASS,
    })
    await assertNoStuckProgress(page, 'admin-gpu-catalog')
    await auditAccessibility(page, 'admin-gpu-catalog', testInfo)
    adminGuards.assertCleanConsole('admin-gpu-catalog')

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
        gpuMode: resourceRequest.gpuMode,
        gpuClass: resourceRequest.gpuClass,
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
