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

const WORK_PROVIDER_BINDING = 'kubernetes-work-local-hostpath'
const PACKAGE_CONTENT = '# LabWeaver live Work fixture\n\nUse the managed environment.\n'

const FULL_CHAIN_TIMEOUT_MS = 1_800_000

test.describe.configure({ timeout: FULL_CHAIN_TIMEOUT_MS })

function diagnosticCode(value) {
  return value?.diagnosticCode ?? value?.diagnostic_code ?? 'diagnostic missing'
}

function terminalRunState(value) {
  return ['succeeded', 'partially_succeeded', 'failed', 'cancelled'].includes(value)
}

async function publishWorkTemplate(page, project) {
  await page.goto(`/researcher/software?projectId=${encodeURIComponent(project.id)}`, {
    waitUntil: 'domcontentloaded',
  })
  await selectProjectByUi(page, project.id)
  await page.getByRole('button', { name: '生成 Work 模板', exact: true }).click()
  await expect(page.getByRole('heading', { name: '生成 Work 模板', exact: true })).toBeVisible()

  const fileInput = page.getByTestId('work-template-file-input')
  const packageDirectory = await mkdtemp(join(tmpdir(), 'labweaver-work-package-'))
  try {
    await writeFile(join(packageDirectory, 'README.md'), PACKAGE_CONTENT, 'utf8')
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
      300_000,
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
      300_000,
    )
    if (candidate.candidate?.spec?.class !== 'work') throw new Error('WORK_TEMPLATE_CANDIDATE_CLASS_INVALID')
    if (candidate.build?.state !== 'succeeded' || !candidate.imageArtifact) {
      throw new Error(`WORK_TEMPLATE_CANDIDATE_BUILD_FAILED:${candidate.build?.diagnosticCode ?? 'artifact missing'}`)
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
    await rm(packageDirectory, { recursive: true, force: true })
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
    await expect(page.getByRole('heading', { name: '资源审批与 Lease 管理', exact: true })).toBeVisible()
    const row = page.locator('tbody tr').filter({ hasText: requestBody.requestKey })
    await expect(row).toHaveCount(1, { timeout: 120_000 })
    await row.click()
    const targetEnvironmentValue = page
      .locator('.request-detail .meta-row')
      .filter({ has: page.locator('.meta-label', { hasText: /^目标$/ }) })
      .locator('code.meta-value')
    await expect(targetEnvironmentValue).toHaveCount(1)
    await expect(targetEnvironmentValue).toHaveText(requestBody.target.environmentId)
    await page.getByLabel('资源申请操作理由').fill('已确认项目 Work 发布版本与 CPU 容量申请。')
    await page.getByLabel('执行后端绑定').fill(WORK_PROVIDER_BINDING)
    await page.getByLabel('批准时长（秒）').fill(String(requestBody.durationSeconds))
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

test('student provisions a Work environment, configures it, and releases its capacity', async ({ page, browser, baseURL }) => {
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')

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

  const project = await createProjectByUi(page, `live-work-${Date.now()}-${uuidv7().slice(0, 8)}`)
  await selectProjectByUi(page, project.id)
  await createProjectPolicy(page.request, baseURL, project.id)
  const { packageData, release } = await publishWorkTemplate(page, project)

  await page.goto(`/researcher/resources?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: '资源申请', exact: true, level: 2 })).toBeVisible()
  await selectProjectByUi(page, project.id)
  const releaseSelect = page.getByLabel('已发布版本')
  await expect(releaseSelect.locator(`option[value="${release.id}:${release.version}"]`)).toHaveCount(1, { timeout: 120_000 })
  await releaseSelect.selectOption(`${release.id}:${release.version}`)
  await page.getByLabel('CPU（millicores）').fill('1000')
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
  const environmentId = requestBody.target.environmentId

  const approval = await approveResourceRequest(browser, baseURL, requestBody)
  const activeRequest = await waitForResourceRequest(page.request, project.id, accepted.requestId)
  expect(activeRequest).toMatchObject({ id: accepted.requestId, projectId: project.id, state: 'active' })
  const lease = await waitForLease(page.request, project.id, accepted.requestId)
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
  await expect(page.locator('.resource-title-row').getByRole('heading', { name: environmentId, exact: true })).toBeVisible({ timeout: 120_000 })
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
  expect(runtimeBody).toContain('Welcome to nginx')

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
  await expectJson(await planResponsePromise, 'WORK_CONFIGURATION_PLAN_LOAD_FAILED')
  await expect(page.getByRole('heading', { name: 'Work 配置计划审核', exact: true })).toBeVisible({ timeout: 300_000 })
  await expect(page.locator('.plan-code').getByRole('heading', { name: '配置脚本', exact: true, level: 5 })).toBeVisible()
  await page.getByLabel('批准原因').fill('已审阅 Work 配置脚本及目标环境，允许执行。')
  const restartConfirmation = page.getByLabel(/我确认执行前后 Work 环境会重启/)
  if (await restartConfirmation.isVisible()) await restartConfirmation.check()
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
  await waitForEnvironment(page.request, environmentId, 'stopped')
  const revokedAccessGrant = await pollJson(
    page.request,
    `/api/v1/access-grants/${accessGrant.id}`,
    (value) => ['revoked', 'expired', 'denied'].includes(value.state),
    'WORK_ACCESS_GRANT_REVOKE_STATUS_FAILED',
    120_000,
  )
  expect(revokedAccessGrant).toMatchObject({
    id: accessGrant.id,
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
  const deniedConnection = await requestNewConnectionAfterLeaseRevoke(
    page.request,
    baseURL,
    project.id,
    environment,
    endpointIds,
  )
  expect(deniedConnection).toMatchObject({
    state: 'denied',
    reasonCode: 'LW_ACCESS_ENDPOINT_ELIGIBILITY_DENIED',
  })
})
