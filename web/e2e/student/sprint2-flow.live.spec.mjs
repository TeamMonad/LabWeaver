import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
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
  assertRealWorkCharges,
  configureRealWorkBudgetByUi,
  createRealWorkPackage,
  ensureRealWorkRates,
  inspectRealWorkFinanceByUi,
  realWorkConfig,
  realWorkResumeConfig,
  readResumablePublishedWork,
  waitForRealWorkCharges,
} from '../support/real-work.mjs'
import { assertNoStuckProgress, auditAccessibility, installUsabilityGuards } from '../support/usability.mjs'

const WORK_PROVIDER_BINDING =
  process.env.LABWEAVER_E2E_PROVIDER_BINDING ?? 'kubernetes-work-local-hostpath'
const PACKAGE_CONTENT = '# LabWeaver live Work fixture\n\nUse the managed environment.\n'

// The agent worker runs one reserved dispatch at a time, so a journey can sit
// behind earlier runs before its own authoring starts. These ceilings cover a
// queued run plus the deployment's own fifteen minute per-candidate LLM bound
// and the image build that follows it.
const FULL_CHAIN_TIMEOUT_MS = 14_400_000
const AUTHORING_RUN_TIMEOUT_MS = 9_000_000
const CANDIDATE_BUILD_TIMEOUT_MS = 3_600_000
const REAL_WORK_CONFIG = realWorkConfig()
const REAL_WORK_RESUME = realWorkResumeConfig()
const REAL_WORK_MODE = Boolean(REAL_WORK_CONFIG || REAL_WORK_RESUME)

test.describe.configure({ timeout: FULL_CHAIN_TIMEOUT_MS })

function diagnosticCode(value) {
  return value?.diagnosticCode ?? value?.diagnostic_code ?? 'diagnostic missing'
}

function terminalRunState(value) {
  return ['succeeded', 'partially_succeeded', 'failed', 'cancelled'].includes(value)
}

async function publishWorkTemplate(page, project, packageCopy = null) {
  await page.goto(`/researcher/software?projectId=${encodeURIComponent(project.id)}`, {
    waitUntil: 'domcontentloaded',
  })
  await selectProjectByUi(page, project.id)
  await page.getByRole('button', { name: '生成 Work 模板', exact: true }).click()
  await expect(page.getByRole('heading', { name: '生成 Work 模板', exact: true })).toBeVisible()

  const fileInput = page.getByTestId('work-template-file-input')
  const packageDirectory = packageCopy?.directory ?? await mkdtemp(join(tmpdir(), 'labweaver-work-package-'))
  const ownsPackageDirectory = !packageCopy
  try {
    if (ownsPackageDirectory) await writeFile(join(packageDirectory, 'README.md'), PACKAGE_CONTENT, 'utf8')
    await fileInput.setInputFiles(packageDirectory)
    await expect(page.getByRole('list', { name: '待上传材料文件', exact: true })).toContainText('README.md')

    const packageResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && /\/api\/v1\/projects\/[^/]+\/problem-package-uploads\/[^/]+\/complete$/.test(url.pathname)
    })
    await page.getByRole('button', { name: '上传材料包', exact: true }).click()
    const packageResponse = await packageResponsePromise
    const packageData = await expectJson(packageResponse, 'WORK_PACKAGE_UPLOAD_FAILED')
    expect(packageData).toMatchObject({ projectId: project.id, revision: expect.any(Number) })
    await expect(page.locator('.package-summary').getByText(/材料包已归档：/)).toBeVisible({ timeout: 120_000 })

    const runResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${project.id}/agent-runs`
    })
    await page.getByRole('button', { name: '启动 Work AgentRun', exact: true }).click()
    const runResponse = await runResponsePromise
    const acceptedRun = await expectJson(runResponse, 'WORK_TEMPLATE_RUN_CREATE_FAILED')
    expect(acceptedRun).toMatchObject({ id: expect.any(String), projectId: project.id })
    expect(acceptedRun.purpose?.environmentClass ?? acceptedRun.environmentClass).toBe('work')

    const run = await pollJson(
      page.request,
      `/api/v1/projects/${project.id}/agent-runs/${acceptedRun.id}`,
      (value) => terminalRunState(value.state),
      'WORK_TEMPLATE_RUN_STATUS_FAILED',
      // A real authoring run drives the sandbox CLI against the deployment's
      // model, so it can take as long as the harness LLM timeout allows.
      AUTHORING_RUN_TIMEOUT_MS,
    )
    if (run.state !== 'succeeded') {
      throw new Error(`WORK_TEMPLATE_RUN_FAILED:${run.state}:${run.tracks?.map((track) => track.attempts?.map(diagnosticCode).join(',')).join(';') ?? 'no tracks'}`)
    }
    const environmentTrack = run.tracks.find((track) => track.kind === 'environment')
    if (!environmentTrack?.candidateId) throw new Error('WORK_TEMPLATE_CANDIDATE_MISSING')

    const candidate = await pollEnvironmentCandidate(
      page.request,
      project.id,
      environmentTrack.candidateId,
      (value) => ['succeeded', 'failed', 'cancelled'].includes(value.build?.state),
      'WORK_TEMPLATE_CANDIDATE_BUILD_STATUS_FAILED',
      CANDIDATE_BUILD_TIMEOUT_MS,
    )
    if (candidate.candidate?.spec?.class !== 'work') throw new Error('WORK_TEMPLATE_CANDIDATE_CLASS_INVALID')
    if (candidate.build?.state !== 'succeeded' || !candidate.imageArtifact) {
      throw new Error(`WORK_TEMPLATE_CANDIDATE_BUILD_FAILED:${candidate.build?.diagnosticCode ?? 'artifact missing'}`)
    }
    if (REAL_WORK_MODE) {
      const runtime = candidate.candidate?.spec?.runtime
      expect(runtime).toMatchObject({
        kind: 'container',
        provider_binding: WORK_PROVIDER_BINDING,
        service_port: 8080,
        build_context: {
          artifactId: expect.any(String),
          objectVersion: expect.any(String),
        },
      })
      realContainerArtifact(candidate)
    }

    const candidateCard = page.getByTestId('work-template-candidate')
    await expect(candidateCard).toBeVisible({ timeout: 120_000 })
    await expect(candidateCard).toContainText('构建完成', { timeout: 120_000 })
    await candidateCard.getByTestId('work-template-candidate-confirmation').check()
    await candidateCard.getByPlaceholder('说明为什么批准这个 Work Environment 候选').fill(
      '已核对 Work EnvironmentSpec、容器 artifact 和项目安全约束。',
    )
    const approveButton = candidateCard.getByRole('button', { name: '批准 Environment 候选', exact: true })
    await expect(approveButton).toBeEnabled()
    const approvalResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${project.id}/environment-candidates/${environmentTrack.candidateId}/decisions`
    })
    await approveButton.click()
    const approvalResponse = await approvalResponsePromise
    const approval = await expectJson(approvalResponse, 'WORK_TEMPLATE_CANDIDATE_APPROVAL_FAILED')
    expect(approval).toMatchObject({ id: expect.any(String), candidateId: environmentTrack.candidateId, decision: 'approved' })
    await expect(candidateCard).toContainText(`候选已批准：${approval.id}`)

    const releaseResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${project.id}/environment-template-releases`
    })
    await page.getByTestId('work-template-release-button').click()
    const releaseAccepted = await expectJson(await releaseResponsePromise, 'WORK_TEMPLATE_RELEASE_CREATE_FAILED')
    expect(releaseAccepted).toMatchObject({ operationId: expect.any(String), statusUrl: expect.stringContaining('/environment-template-releases/') })
    const release = await pollJson(
      page.request,
      releaseAccepted.statusUrl,
      (value) => value.id && value.projectId === project.id && value.candidateId === environmentTrack.candidateId,
      'WORK_TEMPLATE_RELEASE_STATUS_FAILED',
      120_000,
    )
    expect(release).toMatchObject({ projectId: project.id, runtimeKind: 'container', version: expect.any(Number) })
    await expect(page.getByTestId('work-template-resource-link')).toBeVisible()
    return { packageData, run, candidate, release }
  } finally {
    if (ownsPackageDirectory) await rm(packageDirectory, { recursive: true, force: true })
  }
}

async function waitForResourceRequest(request, projectId, requestId) {
  const snapshot = await pollJson(
    request,
    `/api/v1/projects/${projectId}/resource-requests`,
    (value) => Array.isArray(value) && value.some((item) => item.id === requestId && ['active', 'rejected', 'cancelled'].includes(item.state)),
    'RESOURCE_REQUEST_STATUS_FAILED',
    240_000,
  )
  const item = snapshot.find((candidate) => candidate.id === requestId)
  if (!item || item.state !== 'active') throw new Error(`RESOURCE_REQUEST_NOT_ACTIVE:${item?.state ?? 'missing'}:${item?.diagnosticCode ?? 'diagnostic missing'}`)
  return item
}

async function waitForLease(request, projectId, requestId) {
  const snapshot = await pollJson(
    request,
    `/api/v1/projects/${projectId}/resource-leases`,
    (value) => Array.isArray(value) && value.some((item) => item.requestId === requestId && ['active', 'revoked', 'expired'].includes(item.state)),
    'RESOURCE_LEASE_STATUS_FAILED',
    240_000,
  )
  const item = snapshot.find((candidate) => candidate.requestId === requestId)
  if (!item || item.state !== 'active') throw new Error(`RESOURCE_LEASE_NOT_ACTIVE:${item?.state ?? 'missing'}:${item?.revokeReasonCode ?? 'diagnostic missing'}`)
  return item
}

async function waitForEnvironment(request, environmentId, expectedState) {
  const value = await pollJson(
    request,
    `/api/v1/environments/${environmentId}`,
    (item) => item.observedState === expectedState || ['failed', 'deleted'].includes(item.observedState),
    `ENVIRONMENT_${expectedState.toUpperCase()}_STATUS_FAILED`,
    240_000,
  )
  if (value.observedState !== expectedState) throw new Error(`ENVIRONMENT_STATE_INVALID:${value.observedState}:${value.lastDiagnosticCode ?? 'diagnostic missing'}`)
  return value
}

async function waitForDeletedEnvironment(request, environmentId) {
  let latest
  await expect.poll(async () => {
    const response = await request.get(`/api/v1/environments/${environmentId}`)
    if (response.status() === 404) return true
    latest = await expectJson(response, 'ENVIRONMENT_CLEANUP_READ_FAILED')
    return latest.observedState === 'deleted'
  }, { timeout: 240_000, intervals: [1000, 2000, 3000] }).toBe(true)
  return latest
}

async function waitForStoppedOrDeletedEnvironment(request, environmentId) {
  const settled = await pollJson(
    request,
    `/api/v1/environments/${environmentId}`,
    (value) => ['stopped', 'failed', 'deleting', 'deleted'].includes(value.observedState),
    'REAL_WORK_CLEANUP_ENVIRONMENT_STOPPED_STATUS_FAILED',
    240_000,
  )
  if (settled.observedState === 'deleting') {
    await waitForDeletedEnvironment(request, environmentId)
    return { ...settled, observedState: 'deleted' }
  }
  if (!['stopped', 'failed', 'deleted'].includes(settled.observedState)) {
    throw new Error(`REAL_WORK_CLEANUP_ENVIRONMENT_STOPPED_STATE_INVALID:${settled.observedState}`)
  }
  return settled
}

async function expectProblem(response, expectedStatus, expectedDiagnostic, label) {
  const bodyText = await response.text()
  let body
  try {
    body = JSON.parse(bodyText)
  } catch (error) {
    throw new Error(`${label}:invalid JSON:${bodyText.slice(0, 2000)}`, { cause: error })
  }
  if (response.status() !== expectedStatus) {
    throw new Error(`${label}:status ${response.status()} expected ${expectedStatus}:${bodyText.slice(0, 2000)}`)
  }
  if (diagnosticCode(body) !== expectedDiagnostic) {
    throw new Error(`${label}:diagnostic ${diagnosticCode(body)} expected ${expectedDiagnostic}`)
  }
  return body
}

async function verifyCrossProjectEnvironmentDenied(browser, baseURL, projectId, environmentId) {
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.teacher })
  try {
    const paths = [
      `/api/v1/environments?projectId=${encodeURIComponent(projectId)}`,
      `/api/v1/environments/${environmentId}`,
      `/api/v1/environments/${environmentId}/endpoints`,
      `/api/v1/projects/${projectId}/resource-requests`,
      `/api/v1/projects/${projectId}/resource-leases`,
    ]
    for (const path of paths) {
      await expectProblem(
        await context.request.get(path),
        403,
        'LW_AUTH_SCOPE_DENIED',
        `CROSS_PROJECT_ENVIRONMENT_ACCESS_DENIED:${path}`,
      )
    }
  } finally {
    await context.close()
  }
}

async function waitForActiveAccessGrant(request, grantId) {
  const grant = await pollJson(
    request,
    `/api/v1/access-grants/${grantId}`,
    (value) => ['active', 'denied', 'expired', 'revoked'].includes(value.state),
    'WORK_ACCESS_GRANT_STATUS_FAILED',
    120_000,
  )
  if (grant.state !== 'active') {
    throw new Error(`WORK_ACCESS_GRANT_NOT_ACTIVE:${grant.state}:${grant.reasonCode ?? 'reason missing'}`)
  }
  return grant
}

async function requestNewConnectionAfterLeaseRevoke(request, baseURL, projectId, environment, endpointIds) {
  const response = await request.post(`/api/v1/environments/${environment.id}/access-grants`, {
    headers: await csrfHeaders(request, baseURL, { 'Idempotency-Key': uuidv7() }),
    data: {
      projectId,
      courseId: null,
      environmentId: environment.id,
      environmentRevision: environment.revision,
      endpointIds,
    },
  })
  const accepted = await expectJson(response, 'POST_REVOKE_NEW_CONNECTION_REQUEST_FAILED')
  expect(accepted).toMatchObject({
    id: expect.any(String),
    projectId,
    environmentId: environment.id,
    environmentRevision: environment.revision,
    state: 'requested',
  })
  const settled = await pollJson(
    request,
    `/api/v1/access-grants/${accepted.id}`,
    (value) => ['denied', 'active', 'expired', 'revoked'].includes(value.state),
    'POST_REVOKE_NEW_CONNECTION_STATUS_FAILED',
    120_000,
  )
  if (settled.state !== 'denied') {
    throw new Error(`POST_REVOKE_NEW_CONNECTION_NOT_DENIED:${settled.state}:${settled.reasonCode ?? 'reason missing'}`)
  }
  if (settled.reasonCode !== 'LW_ACCESS_ENDPOINT_ELIGIBILITY_DENIED') {
    throw new Error(`POST_REVOKE_NEW_CONNECTION_WRONG_REASON:${settled.reasonCode ?? 'reason missing'}`)
  }
  return settled
}

async function approveResourceRequest(browser, baseURL, requestBody) {
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  const page = await context.newPage()
  try {
    await page.goto('/admin/resource-approval', { waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('heading', { name: '资源审批与资源使用授权管理', exact: true })).toBeVisible()
    const row = page.locator('tbody tr').filter({ hasText: requestBody.requestKey })
    await expect(row).toHaveCount(1, { timeout: 120_000 })
    await row.click()
    const targetEnvironmentValue = page
      .locator('.request-detail .meta-row')
      .filter({ has: page.locator('.meta-label', { hasText: /^目标$/ }) })
      .locator('code.meta-value')
    await expect(targetEnvironmentValue).toHaveCount(1)
    await expect(targetEnvironmentValue).toHaveText(requestBody.target.environmentId)
    await page.getByLabel('资源申请操作理由', { exact: true }).fill('已确认项目 Work 发布版本与 CPU 容量申请。')
    await page.getByRole('textbox', { name: '执行后端绑定', exact: true }).fill(WORK_PROVIDER_BINDING)
    await page.getByLabel('批准时长（秒）', { exact: true }).fill(String(requestBody.durationSeconds))
    const approveButton = page.getByRole('button', { name: '批准', exact: true })
    await expect(approveButton).toBeEnabled()
    const responsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/resource-requests/${requestBody.requestId}/approve`
    })
    await approveButton.click()
    await page.getByRole('alertdialog').getByRole('button', { name: '确认', exact: true }).click()
    const response = await responsePromise
    const approval = await expectJson(response, 'RESOURCE_REQUEST_APPROVAL_FAILED')
    expect(approval).toMatchObject({ requestId: requestBody.requestId, leaseId: expect.any(String) })
    return approval
  } finally {
    await context.close()
  }
}

function realContainerArtifact(candidate) {
  const artifact = candidate?.imageArtifact ?? candidate?.build?.artifact
  if (!artifact || artifact.kind !== 'container') throw new Error('REAL_WORK_CONTAINER_ARTIFACT_MISSING')
  if (typeof artifact.repository !== 'string' || artifact.repository.trim() === '') {
    throw new Error('REAL_WORK_CONTAINER_REPOSITORY_MISSING')
  }
  if (!/^sha256:[0-9a-f]{64}$/i.test(artifact.digest ?? '')) {
    throw new Error('REAL_WORK_CONTAINER_DIGEST_INVALID')
  }
  if (artifact.digest.toLowerCase() === REAL_WORK_CONFIG.goldenBaseDigest) {
    throw new Error('REAL_WORK_BUILD_REUSED_GOLDEN_BASE_DIGEST')
  }
  return artifact
}

async function issueWorkAccessGrant(request, baseURL, projectId, environment) {
  const endpoints = await expectJson(
    await request.get(`/api/v1/environments/${environment.id}/endpoints`),
    'REAL_WORK_ENDPOINTS_READ_FAILED',
  )
  if (!Array.isArray(endpoints.items) || endpoints.items.length === 0) throw new Error('REAL_WORK_ENDPOINTS_MISSING')
  const endpointIds = endpoints.items.map((endpoint) => endpoint.id)
  const accepted = await expectJson(
    await request.post(`/api/v1/environments/${environment.id}/access-grants`, {
      headers: await csrfHeaders(request, baseURL, { 'Idempotency-Key': uuidv7() }),
      data: {
        projectId,
        courseId: null,
        environmentId: environment.id,
        environmentRevision: environment.revision,
        endpointIds,
      },
    }),
    'REAL_WORK_ACCESS_GRANT_CREATE_FAILED',
  )
  const grant = await waitForActiveAccessGrant(request, accepted.id)
  const httpGrant = grant.endpointGrants.find(
    (endpointGrant) => (endpointGrant.protocol === 'http' || endpointGrant.protocol === 'https')
      && typeof endpointGrant.connectUrl === 'string',
  )
  if (!httpGrant?.connectUrl) throw new Error('REAL_WORK_ACCESS_GRANT_HTTP_CONNECTION_MISSING')
  return { grant, httpGrant }
}

async function readWorkEndpoint(request, connectUrl, label) {
  const response = await request.get(connectUrl)
  const body = await response.text()
  if (!response.ok()) throw new Error(`${label}:${response.status()}:${body.slice(0, 2000)}`)
  expect(response.status()).toBe(200)
  return body
}

async function readWorkFile(request, connectUrl, fileName, label) {
  const base = connectUrl.endsWith('/') ? connectUrl : `${connectUrl}/`
  return await readWorkEndpoint(request, `${base}${fileName}`, label)
}

function httpEndpointGrant(grant) {
  const httpGrant = grant?.endpointGrants?.find(
    (endpointGrant) => (endpointGrant.protocol === 'http' || endpointGrant.protocol === 'https')
      && typeof endpointGrant.connectUrl === 'string',
  )
  if (!httpGrant?.connectUrl) throw new Error('REAL_WORK_HTTP_ENDPOINT_GRANT_MISSING')
  return httpGrant
}

async function cleanupWorkResources(request, baseURL, projectId, environmentId, leaseId, requestId) {
  if (!leaseId && requestId) {
    const leases = await expectJson(
      await request.get(`/api/v1/projects/${projectId}/resource-leases`),
      'REAL_WORK_CLEANUP_LEASE_LIST_FAILED',
    )
    if (!Array.isArray(leases)) throw new Error('REAL_WORK_CLEANUP_LEASE_LIST_INVALID')
    leaseId = leases.find((lease) => lease.requestId === requestId)?.id ?? null

    if (!leaseId) {
      let trackedRequest = await expectJson(
        await request.get(`/api/v1/resource-requests/${requestId}`),
        'REAL_WORK_CLEANUP_RESOURCE_REQUEST_READ_FAILED',
      )
      if (trackedRequest.state === 'reviewing') {
        const cancelled = await expectJson(
          await request.post(`/api/v1/resource-requests/${requestId}/cancel`, {
            headers: await csrfHeaders(request, baseURL, {
              'Idempotency-Key': uuidv7(),
              'If-Match': `"rev-${trackedRequest.revision}"`,
            }),
            data: {
              expectedRevision: trackedRequest.revision,
              reason: 'real Work E2E cleanup',
            },
          }),
          'REAL_WORK_CLEANUP_RESOURCE_REQUEST_CANCEL_FAILED',
        )
        expect(cancelled).toMatchObject({ requestId })
        trackedRequest = await pollJson(
          request,
          `/api/v1/resource-requests/${requestId}`,
          (value) => ['rejected', 'cancelled'].includes(value.state),
          'REAL_WORK_CLEANUP_RESOURCE_REQUEST_CANCEL_STATUS_FAILED',
          120_000,
        )
        if (trackedRequest.state !== 'cancelled') {
          throw new Error(`REAL_WORK_CLEANUP_RESOURCE_REQUEST_NOT_CANCELLED:${trackedRequest.state}`)
        }
      } else if (trackedRequest.state === 'allocating') {
        trackedRequest = await pollJson(
          request,
          `/api/v1/resource-requests/${requestId}`,
          (value) => value.state !== 'allocating',
          'REAL_WORK_CLEANUP_RESOURCE_REQUEST_ALLOCATION_STATUS_FAILED',
          120_000,
        )
        if (trackedRequest.state === 'active' || trackedRequest.state === 'expiring') {
          const allocatedLeases = await expectJson(
            await request.get(`/api/v1/projects/${projectId}/resource-leases`),
            'REAL_WORK_CLEANUP_LEASE_LIST_AFTER_ALLOCATION_FAILED',
          )
          if (!Array.isArray(allocatedLeases)) throw new Error('REAL_WORK_CLEANUP_LEASE_LIST_AFTER_ALLOCATION_INVALID')
          leaseId = allocatedLeases.find((lease) => lease.requestId === requestId)?.id ?? null
          if (!leaseId) throw new Error(`REAL_WORK_CLEANUP_LEASE_MISSING:${trackedRequest.state}`)
        }
      }
      if (!leaseId && !['rejected', 'cancelled', 'expired'].includes(trackedRequest.state)) {
        throw new Error(`REAL_WORK_CLEANUP_RESOURCE_REQUEST_UNSAFE_WITHOUT_LEASE:${trackedRequest.state}`)
      }
    }
  }

  if (environmentId) {
    const currentResponse = await request.get(`/api/v1/environments/${environmentId}`)
    if (currentResponse.status() !== 404) {
      let current = await expectJson(currentResponse, 'REAL_WORK_CLEANUP_ENVIRONMENT_READ_FAILED')
      if (current.observedState !== 'deleted') {
        if (['requested', 'validating', 'building', 'provisioning', 'updating'].includes(current.observedState)) {
          current = await pollJson(
            request,
            `/api/v1/environments/${environmentId}`,
            (value) => ['ready', 'stopped', 'failed', 'deleting', 'deleted'].includes(value.observedState),
            'REAL_WORK_CLEANUP_ENVIRONMENT_PROVISION_FAILED',
            240_000,
          )
        }
        if (current.observedState === 'ready') {
          const stopAccepted = await expectJson(
            await request.post(`/api/v1/environments/${environmentId}/stop`, {
              headers: await csrfHeaders(request, baseURL, {
                'Idempotency-Key': uuidv7(),
                'If-Match': `"rev-${current.revision}"`,
              }),
            }),
            'REAL_WORK_CLEANUP_ENVIRONMENT_STOP_FAILED',
          )
          const stopOperation = await pollJson(
            request,
            stopAccepted.statusUrl,
            (value) => ['succeeded', 'failed', 'cancelled'].includes(value.state),
            'REAL_WORK_CLEANUP_ENVIRONMENT_STOP_STATUS_FAILED',
            240_000,
          )
          if (stopOperation.state !== 'succeeded') {
            const afterStopResponse = await request.get(`/api/v1/environments/${environmentId}`)
            if (afterStopResponse.status() === 404) {
              current = { observedState: 'deleted' }
            } else {
              const afterStop = await expectJson(afterStopResponse, 'REAL_WORK_CLEANUP_ENVIRONMENT_STOP_FAILURE_READ_FAILED')
              if (afterStop.observedState === 'deleting') {
                await waitForDeletedEnvironment(request, environmentId)
                current = { observedState: 'deleted' }
              } else {
                throw new Error(`REAL_WORK_CLEANUP_ENVIRONMENT_STOP_OPERATION_FAILED:${stopOperation.state}`)
              }
            }
          }
          if (stopOperation.state === 'succeeded') {
            const afterStopResponse = await request.get(`/api/v1/environments/${environmentId}`)
            current = afterStopResponse.status() === 404
              ? { observedState: 'deleted' }
              : await expectJson(afterStopResponse, 'REAL_WORK_CLEANUP_ENVIRONMENT_STOP_READ_FAILED')
          }
        }
        if (current.observedState === 'deleting') {
          await waitForDeletedEnvironment(request, environmentId)
        } else if (!['stopped', 'failed', 'deleted'].includes(current.observedState)) {
          current = await waitForStoppedOrDeletedEnvironment(request, environmentId)
        }
      }
    }
  }

  if (leaseId) {
    const leaseResponse = await request.get(`/api/v1/resource-leases/${leaseId}`)
    if (leaseResponse.status() !== 404) {
      const lease = await expectJson(leaseResponse, 'REAL_WORK_CLEANUP_LEASE_READ_FAILED')
      if (['active', 'allocating'].includes(lease.state)) {
        const revoked = await expectJson(
          await request.post(`/api/v1/resource-leases/${leaseId}/revoke`, {
            headers: await csrfHeaders(request, baseURL, {
              'Idempotency-Key': uuidv7(),
              'If-Match': `"rev-${lease.revision}"`,
            }),
            data: {
              expectedRevision: lease.revision,
              reason: 'real Work E2E cleanup',
            },
          }),
          'REAL_WORK_CLEANUP_LEASE_REVOKE_FAILED',
        )
        expect(revoked).toMatchObject({ id: leaseId })
      }
      if (['active', 'allocating', 'expiring'].includes(lease.state)) {
        const finalLease = await pollJson(
          request,
          `/api/v1/resource-leases/${leaseId}`,
          (value) => ['revoked', 'expired'].includes(value.state),
          'REAL_WORK_CLEANUP_LEASE_REVOKE_STATUS_FAILED',
          240_000,
        )
        if (!['revoked', 'expired'].includes(finalLease.state)) {
          throw new Error(`REAL_WORK_CLEANUP_LEASE_NOT_TERMINAL:${finalLease.state}`)
        }
      } else if (!['revoked', 'expired'].includes(lease.state)) {
        throw new Error(`REAL_WORK_CLEANUP_LEASE_STATE_INVALID:${lease.state}`)
      }
    }
  }

  if (environmentId) {
    const latestResponse = await request.get(`/api/v1/environments/${environmentId}`)
    if (latestResponse.status() !== 404) {
      const latest = await expectJson(latestResponse, 'REAL_WORK_CLEANUP_ENVIRONMENT_READ_BEFORE_DELETE_FAILED')
      if (['stopped', 'failed'].includes(latest.observedState)) {
        const deleteAccepted = await expectJson(
          await request.post(`/api/v1/environments/${environmentId}/delete`, {
            headers: await csrfHeaders(request, baseURL, {
              'Idempotency-Key': uuidv7(),
              'If-Match': `"rev-${latest.revision}"`,
            }),
          }),
          'REAL_WORK_CLEANUP_ENVIRONMENT_DELETE_FAILED',
        )
        const deleteOperation = await pollJson(
          request,
          deleteAccepted.statusUrl,
          (value) => ['succeeded', 'failed', 'cancelled'].includes(value.state),
          'REAL_WORK_CLEANUP_ENVIRONMENT_DELETE_STATUS_FAILED',
          240_000,
        )
        if (deleteOperation.state !== 'succeeded') throw new Error(`REAL_WORK_CLEANUP_ENVIRONMENT_DELETE_OPERATION_FAILED:${deleteOperation.state}`)
      } else if (!['deleting', 'deleted'].includes(latest.observedState)) {
        throw new Error(`REAL_WORK_CLEANUP_ENVIRONMENT_DELETE_STATE_INVALID:${latest.observedState}`)
      }
      await waitForDeletedEnvironment(request, environmentId)
    }
  }
}

test('student provisions a Work environment, configures it, and releases its capacity', async ({ page, browser, baseURL }) => {
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')
  if (REAL_WORK_RESUME && REAL_WORK_CONFIG) throw new Error('REAL_WORK_RESUME_AND_FULL_PROVIDER_CONFIG_CONFLICT')
  const guards = installUsabilityGuards(page)

  await expectProblem(
    await page.request.get('/api/v1/resource-requests'),
    403,
    'LW_AUTH_SCOPE_DENIED',
    'STUDENT_GLOBAL_RESOURCE_REQUESTS_DENIED',
  )
  await expectProblem(
    await page.request.get('/api/v1/resource-leases'),
    403,
    'LW_AUTH_SCOPE_DENIED',
    'STUDENT_GLOBAL_RESOURCE_LEASES_DENIED',
  )

  const resumed = REAL_WORK_RESUME
    ? await readResumablePublishedWork(page.request, REAL_WORK_RESUME)
    : null
  const project = resumed?.project ?? await createProjectByUi(page, `live-work-${Date.now()}-${uuidv7().slice(0, 8)}`)
  if (resumed) {
    await page.goto(`/researcher/workspaces?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
  }
  await selectProjectByUi(page, project.id)
  if (!resumed) await createProjectPolicy(page.request, baseURL, project.id)
  const packageCopy = REAL_WORK_CONFIG ? await createRealWorkPackage(REAL_WORK_CONFIG.goldenBaseImage) : null
  let trackedEnvironmentId = null
  let trackedLeaseId = null
  let trackedRequestId = null
  try {
    const { packageData, release } = resumed
      ? { packageData: resumed.packageData, release: resumed.release }
      : await publishWorkTemplate(page, project, packageCopy)
    const expectedSeedMarker = packageCopy?.seedMarker ?? resumed?.seedMarker
    const expectedPersistenceMarker = packageCopy?.persistenceMarker ?? resumed?.persistenceMarker

    if (REAL_WORK_MODE) {
      await ensureRealWorkRates(browser, baseURL)
      await configureRealWorkBudgetByUi(browser, baseURL, project.id)
    }

    await page.goto(`/researcher/resources?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('heading', { name: '资源申请', exact: true, level: 2 })).toBeVisible()
    await selectProjectByUi(page, project.id)
    const releaseSelect = page.getByLabel('已发布版本')
    await expect(releaseSelect.locator(`option[value="${release.id}:${release.version}"]`)).toHaveCount(1, { timeout: 120_000 })
    await releaseSelect.selectOption(`${release.id}:${release.version}`)
    await page.getByLabel('CPU（m）').fill('1000')
    await page.getByLabel('时长（小时）').fill('1')
    await page.getByLabel('内存（GiB）').fill('2')
    await page.getByLabel('存储（GiB）').fill('10')
    const resourceResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST' && url.pathname === '/api/v1/resource-requests'
    })
    await page.getByRole('button', { name: '提交资源申请', exact: true }).click()
    const resourceResponse = await resourceResponsePromise
    const accepted = await expectJson(resourceResponse, 'RESOURCE_REQUEST_CREATE_FAILED')
    expect(accepted).toMatchObject({ requestId: expect.any(String) })
    const requestBody = resourceResponse.request().postDataJSON()
    expect(requestBody).toMatchObject({ projectId: project.id, target: { kind: 'environment', releaseId: release.id, releaseVersion: release.version } })
  requestBody.requestId = accepted.requestId
  trackedRequestId = accepted.requestId
    const environmentId = requestBody.target.environmentId
    trackedEnvironmentId = environmentId

    const approval = await approveResourceRequest(browser, baseURL, requestBody)
    trackedLeaseId = approval.leaseId
    const activeRequest = await waitForResourceRequest(page.request, project.id, accepted.requestId)
    expect(activeRequest).toMatchObject({ id: accepted.requestId, projectId: project.id, state: 'active' })
    const lease = await waitForLease(page.request, project.id, accepted.requestId)
    trackedLeaseId = lease.id
    expect(lease).toMatchObject({
      id: approval.leaseId,
      requestId: activeRequest.id,
      claimId: expect.any(String),
      state: 'active',
      revision: expect.any(Number),
    })

    await page.reload({ waitUntil: 'domcontentloaded' })
    const connectLink = page.getByRole('link', { name: '连接', exact: true })
    await expect(connectLink).toBeVisible({ timeout: 120_000 })
    await connectLink.click()
    await expect(page).toHaveURL(new RegExp(`[?&]environmentId=${encodeURIComponent(environmentId)}(?:&|$)`))
    // The console titles a Work environment `work-<environment id>`, so match the
    // rendered heading by containment rather than by an exact id comparison.
    await expect(
      page.locator('.resource-title-row').getByRole('heading', { name: environmentId }),
    ).toBeVisible({ timeout: 120_000 })
    await expect(page.locator('.env-meta-grid')).toContainText('容器')
    const environment = await waitForEnvironment(page.request, environmentId, 'ready')
    expect(environment.class).toBe('work')
    expect(environment.projectId).toBe(project.id)
    expect(environment.providerBinding).toBe(WORK_PROVIDER_BINDING)
    expect(environment.revision).toEqual(expect.any(Number))
    const endpointsResponse = await page.request.get(`/api/v1/environments/${environmentId}/endpoints`)
    const endpoints = await expectJson(endpointsResponse, 'WORK_ENVIRONMENT_ENDPOINTS_READ_FAILED')
    expect(Array.isArray(endpoints.items)).toBe(true)
    if (endpoints.items.length === 0) throw new Error('WORK_ENVIRONMENT_ENDPOINTS_MISSING')
    const endpointIds = endpoints.items.map((endpoint) => endpoint.id)
    expect(new Set(endpointIds).size).toBe(endpointIds.length)
    await verifyCrossProjectEnvironmentDenied(browser, baseURL, project.id, environmentId)

    await expect(page.getByRole('button', { name: '签发访问授权', exact: true })).toBeVisible({ timeout: 120_000 })
    const accessGrantResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/environments/${environmentId}/access-grants`
    })
    await page.getByRole('button', { name: '签发访问授权', exact: true }).click()
    const accessGrantResponse = await accessGrantResponsePromise
    const requestedAccessGrant = await expectJson(accessGrantResponse, 'WORK_ACCESS_GRANT_CREATE_FAILED')
    expect(requestedAccessGrant).toMatchObject({
      id: expect.any(String),
      projectId: project.id,
      environmentId,
      environmentRevision: environment.revision,
      state: 'requested',
    })
    const accessGrant = await waitForActiveAccessGrant(page.request, requestedAccessGrant.id)
    let revocationTargetGrant = accessGrant
    expect(accessGrant).toMatchObject({
      id: requestedAccessGrant.id,
      projectId: project.id,
      environmentId,
      environmentRevision: environment.revision,
      state: 'active',
      revision: expect.any(Number),
    })
    expect(accessGrant.endpointGrants).toHaveLength(endpoints.items.length)
    const endpointsById = new Map(endpoints.items.map((endpoint) => [endpoint.id, endpoint]))
    for (const endpointGrant of accessGrant.endpointGrants) {
      const endpoint = endpointsById.get(endpointGrant.endpointId)
      if (!endpoint) throw new Error(`WORK_ACCESS_GRANT_ENDPOINT_UNKNOWN:${endpointGrant.endpointId}`)
      expect(endpointGrant).toMatchObject({
        accessGrantId: accessGrant.id,
        endpointId: endpoint.id,
        endpointRevision: endpoint.revision,
        protocol: endpoint.protocol,
        action: 'connect',
        health: 'healthy',
        expiresAt: expect.any(String),
      })
      if (endpoint.protocol === 'http' || endpoint.protocol === 'https') {
        expect(endpointGrant.connectUrl).toBe(`/connect/${endpointGrant.id}/`)
      }
    }
    const httpGrant = accessGrant.endpointGrants.find(
      (endpointGrant) => (endpointGrant.protocol === 'http' || endpointGrant.protocol === 'https')
        && typeof endpointGrant.connectUrl === 'string',
    )
    if (!httpGrant?.connectUrl) throw new Error('WORK_ACCESS_GRANT_HTTP_CONNECTION_MISSING')
    const runtimeResponse = await page.request.get(httpGrant.connectUrl)
    const runtimeBody = await runtimeResponse.text()
    if (!runtimeResponse.ok()) {
      throw new Error(`WORK_ACCESS_GRANT_RUNTIME_GET_FAILED:${runtimeResponse.status()}:${runtimeBody.slice(0, 2000)}`)
    }
    expect(runtimeResponse.status()).toBe(200)
    if (REAL_WORK_MODE) {
      const seedBody = await readWorkFile(
        page.request,
        httpGrant.connectUrl,
        'seed.txt',
        'REAL_WORK_SEED_FILE_READ_FAILED',
      )
      expect(seedBody.trim()).toBe(expectedSeedMarker)
    } else {
      expect(runtimeBody).toContain('Welcome to nginx')
    }
    await assertNoStuckProgress(page, 'student-work-environment')
    await auditAccessibility(page, 'student-work-environment')
    guards.assertCleanConsole('student-work-environment')

    await page.goto(`/researcher/software?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
    await selectProjectByUi(page, project.id)
    await expect(page.getByRole('button', { name: '配置现有 Work', exact: true })).toHaveAttribute('aria-pressed', 'true')
    const workEnvironment = page.getByLabel('Work 环境')
    await expect(workEnvironment.locator(`option[value="${environmentId}"]`)).toHaveCount(1, { timeout: 120_000 })
    await workEnvironment.selectOption(environmentId)
    await page.getByLabel('材料包 ID').fill(packageData.id)
    await page.getByLabel('材料包 Revision').fill(String(packageData.revision))
    await page.getByLabel(/我确认 Agent 可能修改该 Work/).check()
    const planResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'GET'
        && url.pathname.startsWith(`/api/v1/projects/${project.id}/agent-runs/`)
        && url.pathname.endsWith('/work-configuration/plan')
    })
    const configurationResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${project.id}/work-configuration-runs`
    })
    await page.getByRole('button', { name: '生成 Work 配置', exact: true }).click()
    const configurationResponse = await configurationResponsePromise
    const configurationRun = await expectJson(configurationResponse, 'WORK_CONFIGURATION_RUN_CREATE_FAILED')
    expect(configurationRun).toMatchObject({ id: expect.any(String), projectId: project.id })
    const configurationPlan = await expectJson(await planResponsePromise, 'WORK_CONFIGURATION_PLAN_LOAD_FAILED')
    expect(configurationPlan).toMatchObject({
      plan: {
        environmentId,
        environmentRevision: expect.any(Number),
        id: expect.any(String),
        requiresRestart: expect.any(Boolean),
        revision: expect.any(Number),
      },
      scriptContent: expect.any(String),
    })
    if (REAL_WORK_MODE) {
      expect(configurationPlan.scriptContent).toContain(expectedPersistenceMarker)
      expect(configurationPlan.verificationScriptContent).toContain('persistence-marker.txt')
    }
    await expect(page.getByRole('heading', { name: 'Work 配置计划审核', exact: true })).toBeVisible({ timeout: 300_000 })
    await expect(page.locator('.plan-code').getByRole('heading', { name: '配置脚本', exact: true, level: 5 })).toBeVisible()
    await page.getByLabel('批准原因').fill('已审阅 Work 配置脚本及目标环境，允许执行。')
    const restartConfirmation = page.getByLabel(/我确认执行前后 Work 环境会重启/)
    if (configurationPlan.plan.requiresRestart) {
      await expect(restartConfirmation).toBeVisible()
      await restartConfirmation.check()
    } else {
      await expect(restartConfirmation).toHaveCount(0)
    }
    const approveConfigurationResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/projects/${project.id}/agent-runs/${configurationRun.id}/work-configuration/approve`
    })
    await page.getByRole('button', { name: '批准并执行 Work 配置', exact: true }).click()
    const approveConfigurationResponse = await approveConfigurationResponsePromise
    const approvedRun = await expectJson(approveConfigurationResponse, 'WORK_CONFIGURATION_APPROVAL_FAILED')
    expect(approvedRun).toMatchObject({ id: configurationRun.id, projectId: project.id })
    const completedConfigurationRun = await pollJson(
      page.request,
      `/api/v1/projects/${project.id}/agent-runs/${configurationRun.id}`,
      (value) => terminalRunState(value.state),
      'WORK_CONFIGURATION_RUN_STATUS_FAILED',
      300_000,
    )
    if (completedConfigurationRun.state !== 'succeeded') throw new Error(`WORK_CONFIGURATION_RUN_FAILED:${completedConfigurationRun.state}`)

    if (REAL_WORK_MODE) {
      const configuredEnvironment = await waitForEnvironment(page.request, environmentId, 'ready')
      const existingGrant = await expectJson(
        await page.request.get(`/api/v1/access-grants/${accessGrant.id}`),
        'REAL_WORK_ACCESS_GRANT_READ_AFTER_CONFIGURATION_FAILED',
      )
      let configuredConnection
      if (existingGrant.state === 'active' && existingGrant.environmentRevision === configuredEnvironment.revision) {
        configuredConnection = { grant: existingGrant, httpGrant: httpEndpointGrant(existingGrant) }
      } else {
        configuredConnection = await issueWorkAccessGrant(page.request, baseURL, project.id, configuredEnvironment)
        revocationTargetGrant = configuredConnection.grant
      }
      const configuredBody = await readWorkEndpoint(
        page.request,
        configuredConnection.httpGrant.connectUrl,
        'REAL_WORK_CONFIGURED_ENDPOINT_READ_FAILED',
      )
      expect(configuredBody).toContain('seed.txt')
      const configuredSeedBody = await readWorkFile(
        page.request,
        configuredConnection.httpGrant.connectUrl,
        'seed.txt',
        'REAL_WORK_CONFIGURED_SEED_FILE_READ_FAILED',
      )
      const configuredPersistenceBody = await readWorkFile(
        page.request,
        configuredConnection.httpGrant.connectUrl,
        'persistence-marker.txt',
        'REAL_WORK_CONFIGURED_PERSISTENCE_FILE_READ_FAILED',
      )
      expect(configuredSeedBody.trim()).toBe(expectedSeedMarker)
      expect(configuredPersistenceBody.trim()).toBe(expectedPersistenceMarker)

      await page.goto(`/researcher/environments?projectId=${encodeURIComponent(project.id)}&environmentId=${encodeURIComponent(environmentId)}`, { waitUntil: 'domcontentloaded' })
      const restartButton = page.getByRole('button', { name: '重启', exact: true })
      await expect(restartButton).toBeEnabled({ timeout: 120_000 })
      const restartResponsePromise = page.waitForResponse((response) => {
        const url = new URL(response.url())
        return response.request().method() === 'POST' && url.pathname === `/api/v1/environments/${environmentId}/restart`
      })
      await restartButton.click()
      const restartAccepted = await expectJson(await restartResponsePromise, 'REAL_WORK_ENVIRONMENT_RESTART_FAILED')
      expect(restartAccepted).toMatchObject({
        environmentId,
        operationId: expect.any(String),
        revision: expect.any(Number),
        statusUrl: expect.stringContaining(`/api/v1/environments/${environmentId}/operations/`),
      })
      const restartOperation = await pollJson(
        page.request,
        restartAccepted.statusUrl,
        (value) => ['succeeded', 'failed', 'cancelled'].includes(value.state),
        'REAL_WORK_ENVIRONMENT_RESTART_OPERATION_STATUS_FAILED',
        240_000,
      )
      expect(restartOperation).toMatchObject({
        environmentId,
        operationId: restartAccepted.operationId,
        kind: 'restart',
        state: 'succeeded',
      })
      const restartedEnvironment = await waitForEnvironment(page.request, environmentId, 'ready')
      const restartedConnection = await issueWorkAccessGrant(page.request, baseURL, project.id, restartedEnvironment)
      revocationTargetGrant = restartedConnection.grant
      const restartedSeedBody = await readWorkFile(
        page.request,
        restartedConnection.httpGrant.connectUrl,
        'seed.txt',
        'REAL_WORK_RESTARTED_SEED_FILE_READ_FAILED',
      )
      const restartedPersistenceBody = await readWorkFile(
        page.request,
        restartedConnection.httpGrant.connectUrl,
        'persistence-marker.txt',
        'REAL_WORK_RESTARTED_PERSISTENCE_FILE_READ_FAILED',
      )
      expect(restartedSeedBody.trim()).toBe(expectedSeedMarker)
      expect(restartedPersistenceBody.trim()).toBe(expectedPersistenceMarker)
    }

    await page.goto(`/researcher/resources?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
    await selectProjectByUi(page, project.id)
    await expect(page.getByRole('button', { name: '续期', exact: true })).toBeVisible({ timeout: 120_000 })
    const renewResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST' && url.pathname === `/api/v1/resource-leases/${lease.id}/renew`
    })
    await page.getByRole('button', { name: '续期', exact: true }).click()
    const renewedLease = await expectJson(await renewResponsePromise, 'RESOURCE_LEASE_RENEW_FAILED')
    expect(renewedLease).toMatchObject({ id: lease.id, state: 'active', revision: expect.any(Number) })
    expect(new Date(renewedLease.expiresAt).getTime()).toBeGreaterThan(new Date(lease.expiresAt).getTime())
    await assertNoStuckProgress(page, 'researcher-resource-lease')
    await auditAccessibility(page, 'researcher-resource-lease')

    await page.goto(`/researcher/environments?projectId=${encodeURIComponent(project.id)}&environmentId=${encodeURIComponent(environmentId)}`, { waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('button', { name: '停止', exact: true })).toBeEnabled({ timeout: 120_000 })
    const stopResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST' && url.pathname === `/api/v1/environments/${environmentId}/stop`
    })
    await page.getByRole('button', { name: '停止', exact: true }).click()
    const stopAccepted = await expectJson(await stopResponsePromise, 'WORK_ENVIRONMENT_STOP_FAILED')
    expect(stopAccepted).toMatchObject({
      environmentId,
      operationId: expect.any(String),
      revision: expect.any(Number),
      statusUrl: expect.stringContaining(`/api/v1/environments/${environmentId}/operations/`),
    })
    const stopOperation = await pollJson(
      page.request,
      stopAccepted.statusUrl,
      (value) => ['succeeded', 'failed', 'cancelled'].includes(value.state),
      'WORK_ENVIRONMENT_STOP_OPERATION_STATUS_FAILED',
      240_000,
    )
    expect(stopOperation).toMatchObject({
      environmentId,
      operationId: stopAccepted.operationId,
      kind: 'stop',
      state: 'succeeded',
    })
    const operationList = await expectJson(
      await page.request.get(`/api/v1/environments/${environmentId}/operations`),
      'WORK_ENVIRONMENT_OPERATIONS_LIST_FAILED',
    )
    expect(operationList).toMatchObject({
      items: expect.any(Array),
      snapshotAt: expect.any(String),
      snapshotSequence: expect.any(String),
    })
    const listedStopOperation = operationList.items.find((item) => item.operationId === stopAccepted.operationId)
    expect(listedStopOperation).toMatchObject({
      environmentId,
      operationId: stopAccepted.operationId,
      kind: 'stop',
      state: 'succeeded',
    })
    await page.reload({ waitUntil: 'domcontentloaded' })
    const operationsTab = page.getByRole('button', { name: '异步操作与诊断', exact: true })
    await expect(operationsTab).toBeVisible({ timeout: 120_000 })
    await operationsTab.click()
    const operationTimelineHeading = page.getByRole('heading', { name: '操作与诊断时间线', exact: true })
    await expect(operationTimelineHeading).toBeVisible({ timeout: 120_000 })
    const operationPane = page.locator('.tab-pane').filter({ has: operationTimelineHeading })
    await expect(operationPane.getByRole('alert')).toHaveCount(0)
    const stoppedEnvironment = await waitForEnvironment(page.request, environmentId, 'stopped')
    const revokedAccessGrant = await pollJson(
      page.request,
      `/api/v1/access-grants/${revocationTargetGrant.id}`,
      (value) => ['revoked', 'expired', 'denied'].includes(value.state),
      'WORK_ACCESS_GRANT_REVOKE_STATUS_FAILED',
      120_000,
    )
    expect(revokedAccessGrant).toMatchObject({
      id: revocationTargetGrant.id,
      state: 'revoked',
      reasonCode: 'environment_stopped',
    })

    await page.goto(`/researcher/resources?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
    await selectProjectByUi(page, project.id)
    await expect(page.getByRole('button', { name: '回收', exact: true })).toBeVisible({ timeout: 120_000 })
    const reclaimResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST' && url.pathname === `/api/v1/resource-leases/${lease.id}/revoke`
    })
    await page.getByRole('button', { name: '回收', exact: true }).click()
    const revokedLease = await expectJson(await reclaimResponsePromise, 'RESOURCE_LEASE_RECLAIM_FAILED')
    expect(revokedLease).toMatchObject({ id: lease.id })
    const finalLease = await pollJson(
      page.request,
      `/api/v1/resource-leases/${lease.id}`,
      (value) => ['revoked', 'expired'].includes(value.state),
      'RESOURCE_LEASE_RECLAIM_STATUS_FAILED',
      240_000,
    )
    if (finalLease.state !== 'revoked') throw new Error(`RESOURCE_LEASE_NOT_REVOKED:${finalLease.state}`)
    await waitForDeletedEnvironment(page.request, environmentId)
    await assertNoStuckProgress(page, 'researcher-resource-released')
    await auditAccessibility(page, 'researcher-resource-released')
    guards.assertCleanConsole('researcher-resource-released')
    if (REAL_WORK_MODE) {
      const finance = await waitForRealWorkCharges(browser, baseURL, project.id)
      assertRealWorkCharges(finance.charges)
      await inspectRealWorkFinanceByUi(browser, baseURL, project.id)
    }
    const deniedConnection = await requestNewConnectionAfterLeaseRevoke(
      page.request,
      baseURL,
      project.id,
      stoppedEnvironment,
      endpointIds,
    )
    expect(deniedConnection).toMatchObject({
      state: 'denied',
      reasonCode: 'LW_ACCESS_ENDPOINT_ELIGIBILITY_DENIED',
    })
  } catch (error) {
    if (!REAL_WORK_MODE) throw error
    try {
      await cleanupWorkResources(page.request, baseURL, project.id, trackedEnvironmentId, trackedLeaseId, trackedRequestId)
    } catch (cleanupError) {
      const primaryMessage = error instanceof Error ? error.message : String(error)
      const cleanupMessage = cleanupError instanceof Error ? cleanupError.message : String(cleanupError)
      throw new Error(`REAL_WORK_PRIMARY_FAILURE:${primaryMessage};REAL_WORK_CLEANUP_FAILED:${cleanupMessage}`, { cause: error })
    }
    throw error
  } finally {
    await packageCopy?.cleanup()
  }
})
