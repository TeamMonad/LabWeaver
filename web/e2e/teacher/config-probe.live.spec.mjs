import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect, test } from '@playwright/test'
import {
  AUTH_STATE, configureProjectPolicyByUi, createProjectByUi,
  expectJson, navigateFromHomeByUi, pollJson, selectProjectByUi, uuidv7,
} from '../support/live.mjs'
import {
  addProjectStudentByUi, cancelAgentRunByUi, readActorId, snapshotProjectResourceRequestIds,
  startExperimentRunByUi, uploadPackageDirectoryByUi,
  waitForAuthoringRunWithResourceApproval,
  waitForFrozenSubmission, waitForProjectEvaluationResultWithResourceApproval,
} from '../support/real-experiment.mjs'
import {
  approveResourceRequestByUi, cancelProjectResourceRequestByUi, releaseProjectLeaseByUi,
} from '../support/real-resource.mjs'
import { createRealWorkSshIdentity, runPinnedSsh } from '../support/real-work-ssh.mjs'
import {
  addSshPublicKeyByUi, deleteSshPublicKeyByUi, issueEnvironmentSshAccessGrantByUi,
} from '../support/ssh-access.mjs'
import { deleteEnvironmentByUi } from '../support/environment-lifecycle.mjs'

const PACKAGE_ROOT = (() => {
  const configured = process.env.LABWEAVER_E2E_CONFIG_PROBE_PACKAGE_DIR
  if (configured === undefined) return join(dirname(fileURLToPath(import.meta.url)), '../../../examples/linux-config-probe')
  const trimmed = configured.trim()
  if (trimmed === '') throw new Error('CONFIG_PROBE_PACKAGE_DIR_INVALID')
  return resolve(trimmed)
})()
const TASK_PROVIDER = process.env.LABWEAVER_E2E_AUTHORING_PROVIDER_BINDING?.trim()
  || process.env.LABWEAVER_E2E_PROVIDER_BINDING?.trim() || 'container-primary-v1'
// The production environment operation deadline is 900 seconds. Keep a
// bounded poll margin so a slow VM import is not cancelled by the harness
// before the backend operation reaches its own deadline.
const ENVIRONMENT_READY_TIMEOUT_MS = 960_000
const TERMINAL_STATES = ['succeeded', 'partially_succeeded', 'failed', 'cancelled']
test.describe.configure({ timeout: 3_600_000, retries: 0 })
test.skip(process.env.LABWEAVER_E2E_CONFIG_PROBE !== '1', 'Opt in to the real VM configuration experiment.')

function configProbeResumeConfig() {
  const projectId = process.env.LABWEAVER_E2E_RESUME_PROJECT_ID?.trim() ?? ''
  const runId = process.env.LABWEAVER_E2E_RESUME_RUN_ID?.trim() ?? ''
  const releaseId = process.env.LABWEAVER_E2E_RESUME_RELEASE_ID?.trim() ?? ''
  const configured = [projectId, runId, releaseId].some((value) => value !== '')
  if (!configured) return null
  if (!projectId || !runId || !releaseId) throw new Error('CONFIG_PROBE_RESUME_PROJECT_RUN_RELEASE_REQUIRED')
  return Object.freeze({ projectId, runId, releaseId })
}

function configProbeBaseDiskMatches(actual, expected) {
  return actual?.binding === expected?.binding
    && actual?.sourceRegistryDigest === expected?.sourceRegistryDigest
    && actual?.capacityBytes === expected?.capacityBytes
}

async function assertConfigProbeBaseDiskCatalog(request, environmentSpec) {
  const baseDisk = environmentSpec.runtime?.base_disk
  if (!baseDisk) throw new Error('CONFIG_PROBE_BASE_DISK_FIXTURE_MISSING')
  const catalog = await expectJson(await request.get('/api/v1/admin/images'), 'CONFIG_PROBE_CATALOG_READ_FAILED')
  const disk = catalog.entries.filter((entry) => entry.binding === baseDisk.binding)
  expect(disk).toHaveLength(1)
  expect(disk[0]).toMatchObject({
    kind: 'virtual_machine',
    status: 'active',
    format: 'qcow2',
    capacityBytes: baseDisk.capacityBytes,
  })
  expect(`docker://${disk[0].sourceReference.split('@')[0].replace(/:[^/:]+$/, '')}@${disk[0].resolvedDigest}`)
    .toBe(baseDisk.sourceRegistryDigest)
}

async function readResumableConfigProbePublication(page, resume, environmentSpec, evaluationSpec, teacherActorId) {
  const { projectId, runId, releaseId } = resume
  const project = await expectJson(
    await page.request.get(`/api/v1/projects/${encodeURIComponent(projectId)}`),
    'CONFIG_PROBE_RESUME_PROJECT_READ_FAILED',
  )
  if (project.id !== projectId || project.ownerActorId !== teacherActorId) {
    throw new Error('CONFIG_PROBE_RESUME_PROJECT_OWNERSHIP_INVALID')
  }

  const run = await expectJson(
    await page.request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/agent-runs/${encodeURIComponent(runId)}`),
    'CONFIG_PROBE_RESUME_RUN_READ_FAILED',
  )
  const environmentTrack = run.tracks?.find((track) => track.kind === 'environment')
  const evaluationTrack = run.tracks?.find((track) => track.kind === 'evaluation')
  if (
    run.id !== runId
    || run.projectId !== projectId
    || run.state !== 'succeeded'
    || run.purpose?.kind !== 'authoring'
    || run.purpose.environmentClass !== 'experiment'
    || !Array.isArray(run.tracks)
    || run.tracks.length !== 2
    || !environmentTrack?.candidateId
    || !evaluationTrack?.candidateId
  ) {
    throw new Error('CONFIG_PROBE_RESUME_RUN_INVALID')
  }

  const [environmentCandidateView, evaluationCandidateView] = await Promise.all([
    expectJson(
      await page.request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/environment-candidates/${encodeURIComponent(environmentTrack.candidateId)}`),
      'CONFIG_PROBE_RESUME_ENVIRONMENT_CANDIDATE_READ_FAILED',
    ),
    expectJson(
      await page.request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/evaluation-candidates/${encodeURIComponent(evaluationTrack.candidateId)}`),
      'CONFIG_PROBE_RESUME_EVALUATION_CANDIDATE_READ_FAILED',
    ),
  ])
  const environment = environmentCandidateView.candidate
  const evaluation = evaluationCandidateView.candidate
  if (
    environment?.id !== environmentTrack.candidateId
    || environment.projectId !== projectId
    || environment.runId !== runId
    || environment.spec?.class !== environmentSpec.class
    || (environmentCandidateView.build != null && environmentCandidateView.build.state !== 'succeeded')
    || evaluation?.id !== evaluationTrack.candidateId
    || evaluation.projectId !== projectId
    || evaluation.runId !== runId
  ) {
    throw new Error('CONFIG_PROBE_RESUME_CANDIDATE_SCOPE_INVALID')
  }
  for (const field of ['class', 'resources', 'network', 'entries', 'security', 'runtime']) {
    expect(environment.spec?.[field], `CONFIG_PROBE_RESUME_ENVIRONMENT_CHANGED:${field}`).toEqual(environmentSpec[field])
  }
  expect(evaluation.spec?.spec).toEqual(evaluationSpec.spec)

  const expectedArtifact = environmentSpec.runtime.base_disk
  const artifact = environmentCandidateView.imageArtifact
  if (
    artifact?.kind !== 'virtual_machine'
    || artifact.format !== 'qcow2'
    || !configProbeBaseDiskMatches(artifact.base_disk, expectedArtifact)
  ) {
    throw new Error('CONFIG_PROBE_RESUME_BASE_DISK_INVALID')
  }

  const release = await expectJson(
    await page.request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/environment-template-releases/${encodeURIComponent(releaseId)}`),
    'CONFIG_PROBE_RESUME_RELEASE_READ_FAILED',
  )
  if (
    release.id !== releaseId
    || release.projectId !== projectId
    || release.agentRunId !== runId
    || release.candidateId !== environment.id
    || release.candidateRevision !== environment.revision
    || release.runtimeKind !== 'virtual_machine'
    || typeof release.publishedAt !== 'string'
    || release.withdrawal != null
    || release.approval?.decision !== 'approved'
    || release.approval?.candidateId !== environment.id
    || release.approval?.candidateRevision !== environment.revision
    || !configProbeBaseDiskMatches(release.artifact?.base_disk, expectedArtifact)
  ) {
    throw new Error('CONFIG_PROBE_RESUME_RELEASE_INVALID')
  }
  const approvalId = release.approval?.id
  if (typeof approvalId !== 'string' || approvalId.trim() === '') {
    throw new Error('CONFIG_PROBE_RESUME_RELEASE_APPROVAL_MISSING')
  }
  const publication = await expectJson(
    await page.request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/authoring-approvals/${encodeURIComponent(approvalId)}`),
    'CONFIG_PROBE_RESUME_PUBLICATION_READ_FAILED',
  )
  if (
    publication.status !== 'ready'
    || publication.approval?.id !== approvalId
    || publication.approval?.projectId !== projectId
    || publication.approval?.environmentCandidateId !== environment.id
    || publication.approval?.evaluationCandidateId !== evaluation.id
    || publication.approval?.environmentCandidateRevision !== environment.revision
    || publication.approval?.evaluationCandidateRevision !== evaluation.revision
    || publication.environmentReleaseId !== releaseId
    || typeof publication.evaluationReleaseId !== 'string'
    || publication.evaluationReleaseId.trim() === ''
  ) {
    throw new Error('CONFIG_PROBE_RESUME_PUBLICATION_INVALID')
  }
  return { project, run, publication }
}

async function ensureProjectStudentByUi(page, projectId, studentActorId) {
  const memberships = await expectJson(
    await page.request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/members`),
    'CONFIG_PROBE_PROJECT_MEMBERS_READ_FAILED',
  )
  const existing = memberships.find((membership) => (
    membership.projectId === projectId
    && membership.actorId === studentActorId
    && membership.role === 'student'
    && membership.state === 'active'
  ))
  if (existing) return existing
  return await addProjectStudentByUi(page, projectId)
}

async function authorAndPublish(page, adminPage, project, environmentSpec, evaluationSpec, teacherActorId, onRun) {
  await configureProjectPolicyByUi(page, project.id, { maxTransientRetries: 0 })
  await page.goto(`/teacher/materials?projectId=${encodeURIComponent(project.id)}`, { waitUntil: 'domcontentloaded' })
  await selectProjectByUi(page, project.id)
  const packageData = await uploadPackageDirectoryByUi(page, PACKAGE_ROOT, 'materials/application.conf')
  const accepted = await startExperimentRunByUi(page, project.id)
  onRun(accepted)
  const run = await waitForAuthoringRunWithResourceApproval({
    request: page.request,
    adminPage,
    projectId: project.id,
    runId: accepted.id,
    requesterId: teacherActorId,
    providerBinding: TASK_PROVIDER,
    timeout: 1_800_000,
    validateRun: (current) => {
      if (current.packageId !== packageData.id || current.purpose?.kind !== 'authoring'
        || current.purpose.environmentClass !== 'experiment') {
        throw new Error('CONFIG_PROBE_AUTHORING_RUN_SCOPE_INVALID')
      }
    },
  })
  if (run.state !== 'succeeded' || run.packageId !== packageData.id) {
    throw new Error(`CONFIG_PROBE_AUTHORING_FAILED:${run.state}`)
  }
  const candidates = {}
  for (const kind of ['environment', 'evaluation']) {
    const track = run.tracks.find((item) => item.kind === kind)
    if (!track?.candidateId) throw new Error('CONFIG_PROBE_CANDIDATE_MISSING')
    candidates[kind] = await expectJson(
      await page.request.get(`/api/v1/projects/${project.id}/${kind}-candidates/${track.candidateId}`),
      'CONFIG_PROBE_CANDIDATE_READ_FAILED',
    )
    expect(candidates[kind].candidate).toMatchObject({ id: track.candidateId, projectId: project.id, runId: run.id })
  }
  const actual = candidates.environment.candidate.spec
  for (const field of ['class', 'resources', 'network', 'entries', 'security', 'runtime']) {
    expect(actual[field], `CONFIG_PROBE_ENVIRONMENT_CHANGED:${field}`).toEqual(environmentSpec[field])
  }
  expect(candidates.evaluation.candidate.spec.spec).toEqual(evaluationSpec.spec)
  const artifact = candidates.environment.imageArtifact
  expect(artifact).toMatchObject({ kind: 'virtual_machine', format: 'qcow2', base_disk: environmentSpec.runtime.base_disk })
  await page.goto(`/teacher/approvals?projectId=${project.id}&runId=${run.id}`, { waitUntil: 'domcontentloaded' })
  await selectProjectByUi(page, project.id)
  await expect(page.getByRole('heading', { name: '实验包批准', exact: true })).toBeVisible()
  await expect(page.locator('input[aria-label="AgentRun ID"]')).toHaveValue(run.id)
  await expect(page.locator('.candidate-card')).toHaveCount(2, { timeout: 120_000 })
  const approve = page.getByRole('button', { name: '批准完整实验包', exact: true })
  await expect(approve).toBeDisabled()
  await page.getByRole('checkbox').check()
  await page.locator('textarea.reason-input').fill('已核对 VM 绑定、仅报告冻结清单与只读实时配置检查，保留原始内容和权限评分要求。')
  await expect(approve).toBeEnabled({ timeout: 240_000 })
  const approvalResponse = page.waitForResponse((response) => response.request().method() === 'POST'
    && new URL(response.url()).pathname === `/api/v1/projects/${project.id}/authoring-approvals`)
  await approve.click()
  const approval = await expectJson(await approvalResponse, 'CONFIG_PROBE_APPROVAL_FAILED')
  expect(approval.imageArtifact).toEqual(artifact)
  expect(approval).toMatchObject({
    projectId: project.id, packageId: packageData.id,
    environmentCandidateId: candidates.environment.candidate.id,
    evaluationCandidateId: candidates.evaluation.candidate.id,
  })
  const publication = await pollJson(page.request,
    `/api/v1/projects/${project.id}/authoring-approvals/${approval.id}`,
    (value) => ['ready', 'failed'].includes(value.status), 'CONFIG_PROBE_PUBLICATION_STATUS_FAILED', 600_000)
  if (publication.status !== 'ready') throw new Error('CONFIG_PROBE_PUBLICATION_FAILED')
  await expect(page.locator('.publication-status')).toHaveAttribute('data-status', 'ready', { timeout: 120_000 })
  return publication
}

async function startStudentEnvironment(page, adminPage, projectId, releaseId, actorId, providerBinding, onAccepted) {
  if (page.url() === 'about:blank') await navigateFromHomeByUi(page, '我的实验')
  else await page.goto(`/student/labs?projectId=${projectId}`, { waitUntil: 'domcontentloaded' })
  await selectProjectByUi(page, projectId)
  await page.getByRole('button', { name: /创建项目环境/ }).first().click()
  const dialog = page.getByRole('dialog', { name: '创建项目环境', exact: true })
  await expect(dialog).toBeVisible()
  const card = dialog.locator('.release-card').filter({ hasText: releaseId })
  await expect(card).toHaveCount(1, { timeout: 120_000 })
  const createResponse = page.waitForResponse((response) => response.request().method() === 'POST'
    && new URL(response.url()).pathname === '/api/v1/environments')
  await card.getByRole('button', { name: '选择并创建', exact: true }).click()
  const response = await createResponse
  const accepted = await expectJson(response, 'CONFIG_PROBE_ENVIRONMENT_CREATE_FAILED')
  expect(accepted).toMatchObject({ environmentId: expect.any(String), operationId: expect.any(String) })
  onAccepted(accepted.environmentId)
  expect(response.request().postDataJSON()).toMatchObject({ projectId, releaseId })
  return await pollJson(page.request, `/api/v1/environments/${accepted.environmentId}`, async (environment) => {
    expect(environment).toMatchObject({ id: accepted.environmentId, projectId, releaseId })
    if (environment.observedState === 'ready') return true
    if (['failed', 'deleted'].includes(environment.observedState)) throw new Error('CONFIG_PROBE_ENVIRONMENT_FAILED')
    const requests = await expectJson(await adminPage.request.get(`/api/v1/projects/${projectId}/resource-requests`),
      'CONFIG_PROBE_ENVIRONMENT_REQUESTS_READ_FAILED')
    for (const request of requests.filter((item) => item.projectId === projectId && item.requesterId === actorId
      && item.target?.kind === 'environment' && item.target.environmentId === accepted.environmentId && item.state === 'reviewing')) {
      if (request.target.releaseId !== releaseId || request.target.releaseVersion !== environment.releaseVersion
        || !['cpuMillicores', 'memoryBytes', 'storageBytes'].every((field) =>
          Number.isSafeInteger(request.requestedResources?.[field]) && request.requestedResources[field] > 0)
        || request.requestedResources?.gpu != null || !Number.isSafeInteger(request.requestedDurationSeconds)
        || request.requestedDurationSeconds <= 0) throw new Error('CONFIG_PROBE_ENVIRONMENT_REQUEST_SCOPE_INVALID')
      await approveResourceRequestByUi(adminPage, {
        projectId, requestId: request.id, requestKey: request.requestKey, environmentId: accepted.environmentId,
        requesterId: actorId, durationSeconds: request.requestedDurationSeconds, providerBinding,
      })
    }
    return false
  }, 'CONFIG_PROBE_ENVIRONMENT_STATUS_FAILED', ENVIRONMENT_READY_TIMEOUT_MS)
}

async function freezeReport(page, projectId, environment) {
  await page.goto(`/student/environments?projectId=${projectId}&environmentId=${environment.id}`, { waitUntil: 'domcontentloaded' })
  await page.getByRole('button', { name: '实验提交与凭据', exact: true }).click()
  const freeze = page.getByRole('button', { name: '发起冻结提交', exact: true })
  await expect(freeze).toBeEnabled({ timeout: 120_000 })
  await freeze.click()
  const dialog = page.getByRole('dialog', { name: '确认冻结清单', exact: true })
  await expect(dialog).toBeVisible()
  await expect(dialog.locator('.freeze-manifest')).toContainText('report.md')
  await expect(dialog.locator('.freeze-manifest')).not.toContainText('application.conf')
  const responsePromise = page.waitForResponse((response) => response.request().method() === 'POST'
    && new URL(response.url()).pathname === `/api/v1/environments/${environment.id}/freeze`)
  await dialog.getByRole('button', { name: '确认冻结', exact: true }).click()
  const accepted = await expectJson(await responsePromise, 'CONFIG_PROBE_FREEZE_FAILED')
  const match = accepted.statusUrl?.match(/^\/api\/v1\/projects\/([^/]+)\/frozen-submissions\/([0-9a-f-]{36})$/)
  if (match?.[1] !== projectId) throw new Error('CONFIG_PROBE_FREEZE_SCOPE_INVALID')
  const frozen = await waitForFrozenSubmission(page.request, projectId, match[2], 'report.md')
  expect(frozen.files.map((file) => file.path)).toEqual(['report.md'])
  expect(frozen.environment).toMatchObject({
    environmentId: environment.id, environmentRevision: environment.revision,
    releaseId: environment.releaseId, releaseVersion: environment.releaseVersion, runtimeKind: 'virtual_machine',
  })
  await expect(page.locator('.evidence-card')).toContainText(frozen.id, { timeout: 120_000 })
  return frozen
}

function guestProgram(workspace, content, mode, createReport) {
  return `import os, pathlib, pwd, subprocess, sys
import apt
assert sys.version_info >= (3, 9), "CONFIG_PROBE_PYTHON_UNSUPPORTED"
home = pwd.getpwuid(os.getuid()).pw_dir
workspace = pathlib.Path(${JSON.stringify(workspace)})
assert os.getuid() != 0 and workspace == pathlib.Path(home) / "workspace", "CONFIG_PROBE_GUEST_WORKSPACE_MISMATCH"
assert subprocess.run(["dpkg-query", "--show", "--showformat=\u0024{db:Status-Status}", "openssh-server"], capture_output=True, text=True).stdout == "installed", "CONFIG_PROBE_OPENSSH_MISSING"
assert subprocess.run(["systemctl", "is-active", "ssh.service"], capture_output=True, text=True).stdout.strip() == "active", "CONFIG_PROBE_SSH_INACTIVE"
workspace.mkdir(exist_ok=True)
configuration = workspace / "application.conf"
configuration.write_text(${JSON.stringify(content)}, encoding="utf8")
configuration.chmod(0o${mode})
${createReport ? 'report = workspace / "report.md"\nreport.write_text("# Configuration experiment\\n\\nConfiguration is checked in the running VM; this report is identical for both submissions.\\n", encoding="utf8")' : ''}
print("CONFIG_PROBE_GUEST_CHANGE_COMPLETE")
`
}

async function assertResult(page, adminPage, projectId, actorId, frozen, releaseId, expectedScore, beforeRequests) {
  const result = await waitForProjectEvaluationResultWithResourceApproval({
    request: page.request, adminPage, projectId, studentActorId: actorId,
    frozenSubmissionId: frozen.id, existingRequestIds: beforeRequests,
  })
  expect(result).toMatchObject({ projectId, frozenSubmissionId: frozen.id, releaseId, maxScore: 100, awardedScore: expectedScore })
  expect(result.steps).toHaveLength(3)
  for (const position of [1, 2]) expect(result.steps.find((step) => step.position === position)).toMatchObject({ role: 'gate', state: 'succeeded' })
  expect(result.steps.find((step) => step.role === 'score')).toMatchObject({
    state: 'succeeded', maxScore: 100, awardedScore: expectedScore,
  })
  await page.goto(`/student/results?projectId=${projectId}`, { waitUntil: 'domcontentloaded' })
  const card = page.locator('.result-card').filter({ hasText: result.runId })
  await expect(card).toHaveCount(1, { timeout: 120_000 })
  await expect(card.locator('.result-score')).toHaveText(`${expectedScore} / 100`)
  return result
}

async function cleanupEnvironment(page, projectId, environmentId) {
  const requests = await expectJson(await page.request.get(`/api/v1/projects/${projectId}/resource-requests`), 'CONFIG_PROBE_CLEANUP_REQUESTS_FAILED')
  const owned = requests.filter((request) => request.projectId === projectId && request.target?.kind === 'environment'
    && request.target.environmentId === environmentId)
  for (const request of owned) {
    if (['active', 'expiring'].includes(request.state)) await releaseProjectLeaseByUi(page, { projectId, requestId: request.id })
    else if (!['rejected', 'cancelled', 'expired', 'revoked'].includes(request.state)) {
      await cancelProjectResourceRequestByUi(page, { projectId, requestKey: request.requestKey })
    }
  }
  await deleteEnvironmentByUi(page, {
    routePrefix: 'student',
    projectId,
    environmentId,
    label: 'CONFIG_PROBE_CLEANUP_DELETE',
  })
}

test('teacher publishes a configuration experiment and student repairs live VM facts', async ({ page, browser, baseURL }) => {
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')
  const resume = configProbeResumeConfig()
  const environmentSpec = JSON.parse(await readFile(join(PACKAGE_ROOT, 'environment.yaml'), 'utf8'))
  const evaluationSpec = JSON.parse(await readFile(join(PACKAGE_ROOT, 'evaluation.yaml'), 'utf8'))
  const content = await readFile(join(PACKAGE_ROOT, 'materials/application.conf'), 'utf8')
  const configurationAssertions = evaluationSpec.spec.steps.find((step) => step.id === 'configuration').runner.assertions
  const checksum = configurationAssertions.find((assertion) => assertion.fact.endsWith('.sha256'))
  expect(checksum.expected).toBe(createHash('sha256').update(content).digest('hex'))
  const path = configurationAssertions.find((assertion) => assertion.fact.endsWith('.exists')).fact.slice(5, -7)
  const workspace = dirname(path)
  const studentContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.student })
  const adminContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  const studentPage = await studentContext.newPage()
  const adminPage = await adminContext.newPage()
  let project, environmentId, run, key, identity
  let primaryFailure
  const cleanupFailures = []
  try {
    const teacherActorId = await readActorId(page.request)
    const studentActorId = await readActorId(studentPage.request)
    await assertConfigProbeBaseDiskCatalog(adminPage.request, environmentSpec)
    let publication
    if (resume) {
      const resumed = await readResumableConfigProbePublication(
        page,
        resume,
        environmentSpec,
        evaluationSpec,
        teacherActorId,
      )
      project = resumed.project
      run = resumed.run
      publication = resumed.publication
    } else {
      project = await createProjectByUi(page, `config-probe-${Date.now()}-${uuidv7().slice(0, 8)}`)
      publication = await authorAndPublish(
        page,
        adminPage,
        project,
        environmentSpec,
        evaluationSpec,
        teacherActorId,
        (accepted) => { run = accepted },
      )
    }
    await ensureProjectStudentByUi(page, project.id, studentActorId)
    identity = await createRealWorkSshIdentity()
    key = await addSshPublicKeyByUi(studentPage, identity, (accepted) => { key = accepted })
    const environment = await startStudentEnvironment(studentPage, adminPage, project.id,
      publication.environmentReleaseId, studentActorId, environmentSpec.runtime.provider_binding,
      (accepted) => { environmentId = accepted })
    expect(environment.gpuAllocation ?? null).toBeNull()
    const connection = await issueEnvironmentSshAccessGrantByUi(studentPage, project.id, environment)
    await runPinnedSsh(connection.endpointGrant, identity, 'python3 -', guestProgram(workspace, 'enabled=false\n', '0600', true))
    const beforeRequests = await snapshotProjectResourceRequestIds(adminPage.request, project.id)
    const before = await freezeReport(studentPage, project.id, environment)
    const beforeResult = await assertResult(studentPage, adminPage, project.id, studentActorId, before, publication.evaluationReleaseId, 0, beforeRequests)
    await runPinnedSsh(connection.endpointGrant, identity, 'python3 -', guestProgram(workspace, content, '0644', false))
    const afterRequests = await snapshotProjectResourceRequestIds(adminPage.request, project.id)
    const after = await freezeReport(studentPage, project.id, environment)
    expect(after.id).not.toBe(before.id)
    expect(after.environment).toEqual(before.environment)
    expect(after.contentSha256).toBe(before.contentSha256)
    const afterResult = await assertResult(studentPage, adminPage, project.id, studentActorId, after, publication.evaluationReleaseId, 100, afterRequests)
    expect(afterResult.runId).not.toBe(beforeResult.runId)
  } catch (error) {
    primaryFailure = error
  } finally {
    if (run && project) {
      try {
        const current = await expectJson(await page.request.get(`/api/v1/projects/${project.id}/agent-runs/${run.id}`), 'CONFIG_PROBE_CLEANUP_RUN_READ_FAILED')
        if (!TERMINAL_STATES.includes(current.state)) {
          await cancelAgentRunByUi(page, {
            projectId: project.id,
            runId: run.id,
            route: '/teacher/materials',
            buttonName: '取消',
            label: 'CONFIG_PROBE_CLEANUP_RUN_CANCEL',
          })
        }
      } catch (error) { cleanupFailures.push(error) }
    }
    if (environmentId && project) {
      try { await cleanupEnvironment(studentPage, project.id, environmentId) } catch (error) { cleanupFailures.push(error) }
    }
    if (key) {
      try { await deleteSshPublicKeyByUi(studentPage, key) } catch (error) { cleanupFailures.push(error) }
    }
    if (identity) {
      try { await identity.cleanup() } catch (error) { cleanupFailures.push(error) }
    }
    await Promise.all([studentContext.close(), adminContext.close()])
  }
  if (primaryFailure || cleanupFailures.length) throw new AggregateError(
    [...(primaryFailure ? [primaryFailure] : []), ...cleanupFailures], 'CONFIG_PROBE_JOURNEY_OR_CLEANUP_FAILED')
})
