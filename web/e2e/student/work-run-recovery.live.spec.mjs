import { expect, test } from '@playwright/test'
import {
  AUTH_STATE,
  expectJson,
  navigateFromHomeByUi,
  pollEnvironmentCandidate,
  pollJson,
  selectProjectByUi,
} from '../support/live.mjs'
import { readActorId } from '../support/real-experiment.mjs'
import {
  WORK_PROVIDER_BINDING,
  approveResourceRequestByUi,
  requestProjectResourceByUi,
} from '../support/real-resource.mjs'
import {
  cleanupWorkResources,
  inspectRealWorkFinanceByUi,
  waitForSettledWorkUsageCharges,
  selectPendingWorkTaskResourceRequest,
} from '../support/real-work.mjs'
import { issueEnvironmentAccessGrantByUi } from '../support/ssh-access.mjs'
import { hasTerminalLine, issueAccessGrantAndConnect, typeTerminalCommand } from '../support/real-gpu.mjs'

const RECOVERY_ENABLED = process.env.LABWEAVER_E2E_WORK_RECOVERY === '1'
const PROJECT_ID = process.env.LABWEAVER_E2E_RECOVERY_PROJECT_ID?.trim() ?? ''
const RUN_ID = process.env.LABWEAVER_E2E_RECOVERY_RUN_ID?.trim() ?? ''
const AUTHORING_RESOURCE_PROVIDER_BINDING =
  process.env.LABWEAVER_E2E_AUTHORING_PROVIDER_BINDING?.trim()
  || process.env.LABWEAVER_E2E_PROVIDER_BINDING?.trim()
  || 'container-primary-v1'

const AUTHORING_TIMEOUT_MS = 9_000_000
const CANDIDATE_TIMEOUT_MS = 3_600_000
const ENVIRONMENT_TIMEOUT_MS = 240_000
const TERMINAL_RUN_STATES = new Set(['succeeded', 'partially_succeeded', 'failed', 'cancelled'])
const ACTIVE_ATTEMPT_STATES = new Set(['pending', 'running', 'repairing', 'awaiting_approval'])

test.describe.configure({ timeout: 14_400_000 })

function requireRecoveryIds() {
  if (!PROJECT_ID || !RUN_ID) throw new Error('LW_WORK_RECOVERY_PROJECT_AND_RUN_REQUIRED')
  if (!/^[0-9a-f-]{36}$/i.test(PROJECT_ID) || !/^[0-9a-f-]{36}$/i.test(RUN_ID)) {
    throw new Error('LW_WORK_RECOVERY_PROJECT_AND_RUN_ID_INVALID')
  }
}

async function assertProjectUsageReadAvailable(adminPage, projectId) {
  const response = await adminPage.request.get(
    `/api/v1/projects/${encodeURIComponent(projectId)}/usage?page=1&pageSize=1`,
  )
  if (response.status() === 404) throw new Error('LW_WORK_RECOVERY_PROJECT_USAGE_READ_UNAVAILABLE')
  const result = await expectJson(response, 'LW_WORK_RECOVERY_PROJECT_USAGE_READ_FAILED')
  if (
    !result
    || !Array.isArray(result.items)
    || result.page !== 1
    || result.pageSize !== 1
    || typeof result.hasMore !== 'boolean'
  ) {
    throw new Error('LW_WORK_RECOVERY_PROJECT_USAGE_READ_INVALID')
  }
  return result
}

function isFullyTerminal(run) {
  return TERMINAL_RUN_STATES.has(run?.state)
    && (run?.tracks ?? []).every((track) => (
      (track.attempts ?? []).every((attempt) => !ACTIVE_ATTEMPT_STATES.has(attempt.state))
    ))
}

function assertWorkAuthoringRun(run, projectId, runId) {
  if (
    run?.id !== runId
    || run.projectId !== projectId
    || run.purpose?.kind !== 'authoring'
    || run.purpose.environmentClass !== 'work'
  ) {
    throw new Error('LW_WORK_RECOVERY_RUN_SCOPE_INVALID')
  }
  return run
}

function environmentTrack(run) {
  const track = run.tracks?.find((item) => item.kind === 'environment')
  if (!track) throw new Error('LW_WORK_RECOVERY_ENVIRONMENT_TRACK_MISSING')
  return track
}

async function readWorkAuthoringRun(request, projectId, runId, label = 'LW_WORK_RECOVERY_RUN_READ_FAILED') {
  const run = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/agent-runs/${encodeURIComponent(runId)}`),
    label,
  )
  return assertWorkAuthoringRun(run, projectId, runId)
}

function assertRetryableFailedRun(run) {
  if (!['failed', 'partially_succeeded'].includes(run.state)) {
    throw new Error(`LW_WORK_RECOVERY_RUN_NOT_FAILED:${run.state}`)
  }
  const attempts = environmentTrack(run).attempts ?? []
  const first = attempts[0]
  if (
    attempts.length !== 1
    || first?.number !== 1
    || !['failed', 'cancelled'].includes(first.state)
    || attempts.some((attempt) => ACTIVE_ATTEMPT_STATES.has(attempt.state))
  ) {
    throw new Error('LW_WORK_RECOVERY_RUN_RETRY_NOT_ALLOWED')
  }
}

async function approvePendingWorkAuthoringResourceByUi(adminPage, {
  projectId,
  runId,
  requesterId,
  approvedRequestIds,
}) {
  const run = await readWorkAuthoringRun(
    adminPage.request,
    projectId,
    runId,
    'LW_WORK_RECOVERY_AUTHORING_RUN_READ_FAILED',
  )
  const track = environmentTrack(run)
  const activeAttempts = (track.attempts ?? []).filter((attempt) => ACTIVE_ATTEMPT_STATES.has(attempt.state))
  if (activeAttempts.length === 0) return
  if (activeAttempts.length !== 1) throw new Error('LW_WORK_RECOVERY_ACTIVE_ATTEMPT_AMBIGUOUS')

  const requests = await expectJson(
    await adminPage.request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/resource-requests`),
    'LW_WORK_RECOVERY_AUTHORING_RESOURCE_REQUESTS_READ_FAILED',
  )
  const request = selectPendingWorkTaskResourceRequest(requests, {
    projectId,
    runId,
    trackKind: 'environment',
    attemptNumber: activeAttempts[0].number,
    studentActorId: requesterId,
    ignoredRequestIds: approvedRequestIds,
  })
  if (!request || request.state !== 'reviewing') return
  if (!Number.isInteger(request.requestedDurationSeconds) || request.requestedDurationSeconds <= 0) {
    throw new Error(`LW_WORK_RECOVERY_AUTHORING_RESOURCE_DURATION_INVALID:${request.id}`)
  }
  await approveResourceRequestByUi(adminPage, {
    requestKey: request.requestKey,
    projectId,
    requestId: request.id,
    requesterId,
    durationSeconds: request.requestedDurationSeconds,
    providerBinding: AUTHORING_RESOURCE_PROVIDER_BINDING,
    onTaskOwnerRelease: async ({ requestId, leaseId }) => {
      if (requestId !== request.id || typeof leaseId !== 'string' || leaseId === '') {
        throw new Error('LW_WORK_RECOVERY_AUTHORING_LEASE_SCOPE_INVALID')
      }
      return true
    },
  })
  approvedRequestIds.add(request.id)
}

async function waitForRetriedAuthoringRun(request, adminPage, projectId, runId, requesterId, minimumRevision) {
  const approvedRequestIds = new Set()
  let latest = null
  await expect.poll(async () => {
    latest = await readWorkAuthoringRun(request, projectId, runId)
    if (latest.revision < minimumRevision) return false
    const retryAttempt = environmentTrack(latest).attempts?.find((attempt) => attempt.number === 2)
    if (!isFullyTerminal(latest) || !retryAttempt || !['succeeded', 'failed', 'cancelled'].includes(retryAttempt.state)) {
      await approvePendingWorkAuthoringResourceByUi(adminPage, {
        projectId,
        runId,
        requesterId,
        approvedRequestIds,
      })
      return false
    }
    return true
  }, { timeout: AUTHORING_TIMEOUT_MS, intervals: [1000, 2000, 3000] }).toBe(true)
  return latest
}

async function waitForEnvironmentReady(request, projectId, environmentId) {
  return await pollJson(
    request,
    `/api/v1/environments/${encodeURIComponent(environmentId)}`,
    (value) => value.projectId === projectId
      && (value.observedState === 'ready' || ['failed', 'deleted'].includes(value.observedState)),
    'LW_WORK_RECOVERY_ENVIRONMENT_READY_STATUS_FAILED',
    ENVIRONMENT_TIMEOUT_MS,
  ).then((value) => {
    if (value.observedState !== 'ready') {
      throw new Error(`LW_WORK_RECOVERY_ENVIRONMENT_NOT_READY:${value.observedState}:${value.lastDiagnosticCode ?? 'diagnostic missing'}`)
    }
    return value
  })
}

async function openExactRunFromHistory(page, projectId, runId) {
  await navigateFromHomeByUi(page, '软件配置')
  await expect(page.getByRole('heading', { name: '软件配置', exact: true })).toBeVisible()
  await selectProjectByUi(page, projectId)
  const history = page.getByTestId('project-agent-run-history')
  await expect(history).toBeVisible({ timeout: 120_000 })
  const row = history.locator('li.run-history-item').filter({ hasText: runId })
  await expect(row).toHaveCount(1, { timeout: 120_000 })
  await row.getByRole('button', { name: '打开任务', exact: true }).click()
  await expect(page).toHaveURL(new RegExp(`[?&]projectId=${encodeURIComponent(projectId)}(?:&|$)`))
  await expect(page).toHaveURL(new RegExp(`[?&]runId=${encodeURIComponent(runId)}(?:&|$)`))
  await expect(page).toHaveURL(/[?&]mode=template(?:&|$)/)
}

async function approveCandidateAndPublishByUi(page, projectId, candidateId) {
  const candidateCard = page.getByTestId('work-template-candidate')
  await expect(candidateCard).toBeVisible({ timeout: 120_000 })
  await expect(candidateCard).toContainText('构建完成', { timeout: 120_000 })

  const approvalForm = candidateCard.getByTestId('work-template-candidate-approval-form')
  if (await approvalForm.count() > 0) {
    await candidateCard.getByTestId('work-template-candidate-confirmation').check()
    await candidateCard.getByPlaceholder('说明为什么批准这个 Work 环境候选').fill('已核对恢复任务生成的 Work 环境规格与运行时产物。')
    const approvalResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${projectId}/environment-candidates/${candidateId}/decisions`
    })
    await approvalForm.getByRole('button', { name: '批准环境候选', exact: true }).click()
    const approval = await expectJson(await approvalResponsePromise, 'LW_WORK_RECOVERY_CANDIDATE_APPROVAL_FAILED')
    expect(approval).toMatchObject({ candidateId, decision: 'approved' })
  } else {
    await expect(candidateCard.getByText(/候选已批准：/)).toBeVisible({ timeout: 120_000 })
  }

  const confirmation = candidateCard.getByTestId('work-template-candidate-confirmation')
  if (await confirmation.count() > 0 && !(await confirmation.isChecked())) await confirmation.check()
  const releaseButton = page.getByTestId('work-template-release-button')
  await expect(releaseButton).toBeVisible({ timeout: 120_000 })
  const releaseResponsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/projects/${projectId}/environment-template-releases`
  })
  await releaseButton.click()
  const accepted = await expectJson(await releaseResponsePromise, 'LW_WORK_RECOVERY_RELEASE_CREATE_FAILED')
  expect(accepted).toMatchObject({ operationId: expect.any(String), statusUrl: expect.stringContaining('/environment-template-releases/') })
  const release = await pollJson(
    page.request,
    accepted.statusUrl,
    (value) => value.projectId === projectId && value.candidateId === candidateId && Number.isInteger(value.version),
    'LW_WORK_RECOVERY_RELEASE_STATUS_FAILED',
    ENVIRONMENT_TIMEOUT_MS,
  )
  await expect(page.getByTestId('work-template-resource-link')).toBeVisible({ timeout: 120_000 })
  return release
}

async function restartEnvironmentByUi(page, projectId, environmentId) {
  await navigateFromHomeByUi(page, '项目与工作空间')
  await expect(page.getByRole('heading', { name: '项目与工作空间', exact: true })).toBeVisible({ timeout: 120_000 })
  await selectProjectByUi(page, projectId)
  const environmentLinks = page.locator('.work-list .work-row a').filter({ hasText: '打开' })
  let targetIndex = -1
  await expect.poll(async () => {
    targetIndex = await environmentLinks.evaluateAll((links, expected) => links.findIndex((link) => {
      const href = link.getAttribute('href')
      if (!href) return false
      const url = new URL(href, window.location.origin)
      return url.pathname === '/researcher/environments'
        && url.searchParams.get('projectId') === expected.projectId
        && url.searchParams.get('environmentId') === expected.environmentId
    }), { projectId, environmentId })
    return targetIndex
  }, { timeout: 120_000, intervals: [500, 1000, 2000] }).toBeGreaterThanOrEqual(0)
  const targetRow = page.locator('.work-list .work-row').nth(targetIndex)
  await expect(targetRow.locator('strong')).toBeVisible()
  await expect(targetRow.locator('strong')).not.toHaveText('')
  await environmentLinks.nth(targetIndex).click()
  await expect.poll(async () => {
    const url = new URL(page.url())
    return url.pathname === '/researcher/environments'
      && url.searchParams.get('projectId') === projectId
      && url.searchParams.get('environmentId') === environmentId
  }, { timeout: 120_000 }).toBe(true)
  await expect(page.getByRole('heading', { name: '项目环境控制台', exact: true })).toBeVisible({ timeout: 120_000 })
  const restartButton = page.getByRole('button', { name: '重启', exact: true })
  await expect(restartButton).toBeEnabled({ timeout: 120_000 })
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/environments/${environmentId}/restart`
  })
  await restartButton.click()
  const accepted = await expectJson(await responsePromise, 'LW_WORK_RECOVERY_ENVIRONMENT_RESTART_FAILED')
  expect(accepted).toMatchObject({ environmentId, operationId: expect.any(String), statusUrl: expect.stringContaining('/operations/') })
  const operation = await pollJson(
    page.request,
    accepted.statusUrl,
    (value) => ['succeeded', 'failed', 'cancelled'].includes(value.state),
    'LW_WORK_RECOVERY_ENVIRONMENT_RESTART_OPERATION_FAILED',
    ENVIRONMENT_TIMEOUT_MS,
  )
  if (operation.state !== 'succeeded') throw new Error(`LW_WORK_RECOVERY_ENVIRONMENT_RESTART_NOT_SUCCEEDED:${operation.state}`)
  return await waitForEnvironmentReady(page.request, projectId, environmentId)
}

async function runWorkTerminalCommand(page, projectId, environmentId, command, marker) {
  const frames = []
  const sockets = new Map()
  const capture = (socket) => {
    const receive = ({ payload }) => {
      frames.push(typeof payload === 'string' ? payload : Buffer.from(payload).toString('utf8'))
    }
    sockets.set(socket, receive)
    socket.on('framereceived', receive)
  }
  page.on('websocket', capture)
  try {
    const terminal = await issueAccessGrantAndConnect(page, projectId, environmentId)
    const output = await typeTerminalCommand(page, terminal.input, frames, command, marker)
    return output
  } finally {
    page.off('websocket', capture)
    for (const [socket, receive] of sockets) socket.off('framereceived', receive)
    frames.length = 0
  }
}

test('student retries the exact failed Work run once and completes its normal environment lifecycle', async ({ page, browser, baseURL }) => {
  test.setTimeout(14_400_000)
  test.skip(!RECOVERY_ENABLED, 'set LABWEAVER_E2E_WORK_RECOVERY=1 with the exact recovery project and run IDs')
  requireRecoveryIds()

  let adminContext = null
  let adminPage = null
  let trackedEnvironmentId = null
  let trackedRequestId = null
  let trackedLeaseId = null
  let baselineChargeIds = new Set()
  let cleanupComplete = false
  let primaryError = null
  try {
    adminContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
    adminPage = await adminContext.newPage()
    // The recovery retry is single-use. Verify the real administrator session
    // can read the usage projection before any retry or resource write.
    await assertProjectUsageReadAvailable(adminPage, PROJECT_ID)
    const baselineCharges = await expectJson(
      await adminPage.request.get(`/api/v1/projects/${encodeURIComponent(PROJECT_ID)}/charges`),
      'LW_WORK_RECOVERY_BASELINE_CHARGES_READ_FAILED',
    )
    if (!Array.isArray(baselineCharges)) throw new Error('LW_WORK_RECOVERY_BASELINE_CHARGES_INVALID')
    baselineChargeIds = new Set(baselineCharges.map((charge) => charge.id).filter((id) => typeof id === 'string' && id !== ''))

    const initialRun = await readWorkAuthoringRun(page.request, PROJECT_ID, RUN_ID)
    assertRetryableFailedRun(initialRun)
    const studentActorId = await readActorId(page.request)

    await openExactRunFromHistory(page, PROJECT_ID, RUN_ID)
    const retry = page.getByRole('button', { name: '重试环境候选生成', exact: true })
    await expect(retry).toBeVisible({ timeout: 120_000 })
    await expect(retry).toBeEnabled()
    const retryResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${PROJECT_ID}/agent-runs/${RUN_ID}/tracks/environment/retry`
    })
    await retry.click()
    const retryResponse = await retryResponsePromise
    const retryHeaders = retryResponse.request().headers()
    expect(retryHeaders['idempotency-key']).toMatch(/^[0-9a-f-]{36}$/i)
    expect(retryHeaders['if-match']).toBe(`"rev-${initialRun.revision}"`)
    const acceptedRetry = await expectJson(retryResponse, 'LW_WORK_RECOVERY_TRACK_RETRY_FAILED')
    expect(acceptedRetry).toMatchObject({ id: RUN_ID, projectId: PROJECT_ID })
    expect(acceptedRetry.revision).toBeGreaterThan(initialRun.revision)

    let run = await waitForRetriedAuthoringRun(
      page.request,
      adminPage,
      PROJECT_ID,
      RUN_ID,
      studentActorId,
      acceptedRetry.revision,
    )
    if (run.state !== 'succeeded') throw new Error(`LW_WORK_RECOVERY_RUN_FAILED_AFTER_RETRY:${run.state}`)
    const candidateId = environmentTrack(run).candidateId
    if (!candidateId) throw new Error('LW_WORK_RECOVERY_CANDIDATE_MISSING')

    const candidate = await pollEnvironmentCandidate(
      page.request,
      PROJECT_ID,
      candidateId,
      async (value) => {
        if (value.candidate?.id !== candidateId || value.candidate.projectId !== PROJECT_ID || value.candidate.runId !== RUN_ID) {
          throw new Error('LW_WORK_RECOVERY_CANDIDATE_SCOPE_INVALID')
        }
        if (!['succeeded', 'failed', 'cancelled'].includes(value.build?.state)) {
          await approvePendingWorkAuthoringResourceByUi(adminPage, {
            projectId: PROJECT_ID,
            runId: RUN_ID,
            requesterId: studentActorId,
            approvedRequestIds: new Set(),
          })
          return false
        }
        return true
      },
      'LW_WORK_RECOVERY_CANDIDATE_STATUS_FAILED',
      CANDIDATE_TIMEOUT_MS,
    )
    if (candidate.build?.state !== 'succeeded' || !candidate.imageArtifact) {
      throw new Error(`LW_WORK_RECOVERY_CANDIDATE_BUILD_FAILED:${candidate.build?.diagnosticCode ?? 'artifact missing'}`)
    }

    await page.reload({ waitUntil: 'domcontentloaded' })
    await expect(page.getByTestId('work-template-candidate')).toBeVisible({ timeout: 120_000 })
    run = await readWorkAuthoringRun(page.request, PROJECT_ID, RUN_ID)
    const release = await approveCandidateAndPublishByUi(page, PROJECT_ID, environmentTrack(run).candidateId)

    const resource = await requestProjectResourceByUi(page, {
      projectId: PROJECT_ID,
      releaseId: release.id,
      releaseVersion: release.version,
      cpuMillicores: 1000,
      memoryGiB: 2,
      storageGiB: 10,
      durationHours: 1,
      onAccepted: ({ requestId, environmentId }) => {
        trackedRequestId = requestId
        trackedEnvironmentId = environmentId
      },
    })
    trackedRequestId = resource.requestId
    trackedEnvironmentId = resource.environmentId
    const approval = await approveResourceRequestByUi(adminPage, {
      requestKey: resource.requestKey,
      projectId: PROJECT_ID,
      requestId: resource.requestId,
      environmentId: resource.environmentId,
      requesterId: studentActorId,
      durationSeconds: resource.durationSeconds,
      providerBinding: WORK_PROVIDER_BINDING,
    })
    trackedLeaseId = approval.leaseId
    let environment = await waitForEnvironmentReady(page.request, PROJECT_ID, resource.environmentId)
    const firstAccess = await issueEnvironmentAccessGrantByUi(page, PROJECT_ID, environment, 'http')
    const firstAccessResponse = await page.request.get(firstAccess.endpointGrant.connectUrl)
    if (!firstAccessResponse.ok()) throw new Error(`LW_WORK_RECOVERY_ACCESS_READ_FAILED:${firstAccessResponse.status()}`)

    const recoveryMarker = `labweaver-recovery-${Date.now()}`
    const initialTerminalOutput = await runWorkTerminalCommand(
      page,
      PROJECT_ID,
      resource.environmentId,
      [
        'set -eu',
        `printf '%s\n' '${recoveryMarker}' > /workspace/recovery-marker.txt`,
        'test -s /workspace/recovery-marker.txt',
        'command -v sh >/dev/null 2>&1',
        'if command -v python3 >/dev/null 2>&1; then python3 --version; elif command -v python >/dev/null 2>&1; then python --version; else exit 41; fi',
        "printf '%s\n' 'LW_WORK_RECOVERY_SOFTWARE_OK'",
      ].join('\n'),
      'LW_WORK_RECOVERY_INITIAL',
    )
    if (!hasTerminalLine(initialTerminalOutput, 'LW_WORK_RECOVERY_SOFTWARE_OK')) {
      throw new Error('LW_WORK_RECOVERY_SOFTWARE_CHECK_MISSING')
    }

    environment = await restartEnvironmentByUi(page, PROJECT_ID, resource.environmentId)
    const restartedAccess = await issueEnvironmentAccessGrantByUi(page, PROJECT_ID, environment, 'http')
    const restartedAccessResponse = await page.request.get(restartedAccess.endpointGrant.connectUrl)
    if (!restartedAccessResponse.ok()) throw new Error(`LW_WORK_RECOVERY_RESTARTED_ACCESS_READ_FAILED:${restartedAccessResponse.status()}`)

    const restartedTerminalOutput = await runWorkTerminalCommand(
      page,
      PROJECT_ID,
      resource.environmentId,
      [
        'set -eu',
        `test "$(cat /workspace/recovery-marker.txt)" = '${recoveryMarker}'`,
        "printf '%s\n' 'LW_WORK_RECOVERY_MARKER_OK'",
      ].join('\n'),
      'LW_WORK_RECOVERY_RESTART',
    )
    if (!hasTerminalLine(restartedTerminalOutput, 'LW_WORK_RECOVERY_MARKER_OK')) {
      throw new Error('LW_WORK_RECOVERY_MARKER_NOT_PERSISTED')
    }

  } catch (error) {
    primaryError = error
  } finally {
    if (!cleanupComplete && (trackedEnvironmentId || trackedLeaseId || trackedRequestId)) {
      try {
        await cleanupWorkResources(
          page.request,
          baseURL,
          PROJECT_ID,
          trackedEnvironmentId,
          trackedLeaseId,
          trackedRequestId,
          page,
        )
        cleanupComplete = true
      } catch (cleanupError) {
        primaryError = primaryError
          ? new AggregateError([primaryError, cleanupError], 'LW_WORK_RECOVERY_PRIMARY_AND_CLEANUP_FAILED')
          : cleanupError
      }
    }
    if (!primaryError && cleanupComplete && trackedRequestId && trackedLeaseId) {
      try {
        const finance = await waitForSettledWorkUsageCharges(browser, baseURL, {
          projectId: PROJECT_ID,
          leases: [{ requestId: trackedRequestId, leaseId: trackedLeaseId }],
          baselineChargeIds,
        })
        await inspectRealWorkFinanceByUi(browser, baseURL, PROJECT_ID, {
          usageRecordIds: finance.matches.map(({ usage }) => usage.id),
          expectedCharges: finance.matches.map(({ charge }) => charge),
        })
      } catch (financeError) {
        primaryError = financeError
      }
    }
    await adminContext?.close()
  }
  if (primaryError) throw primaryError
})
