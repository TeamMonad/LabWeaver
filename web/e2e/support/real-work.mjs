import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect } from '@playwright/test'
import {
  AUTH_STATE,
  csrfHeaders,
  expectJson,
  uuidv7,
} from './live.mjs'

const DIGEST_PINNED_IMAGE = /^[^\s@]+@sha256:([0-9a-f]{64})$/i
const BILLING_UNITS = [
  'cpu_millicore_second',
  'memory_byte_second',
  'storage_byte_second',
]

function requireDigestPinnedImage(value) {
  const image = typeof value === 'string' ? value.trim() : ''
  const match = image.match(DIGEST_PINNED_IMAGE)
  if (!match) throw new Error('LABWEAVER_E2E_SECURITY_BASE_IMAGE_MUST_BE_DIGEST_PINNED')
  return { image, digest: `sha256:${match[1].toLowerCase()}` }
}

/**
 * Resolve the explicit opt-in settings for the real Work provider path.
 * Ordinary Playwright runs continue to use the existing fixture package.
 */
export function realWorkConfig() {
  // An explicit resume continues from immutable IDs supplied by the caller.
  // It must not require another provider/base-image configuration because it
  // deliberately skips the already-paid authoring path.
  const resumeConfigured = [
    process.env.LABWEAVER_E2E_RESUME_PROJECT_ID,
    process.env.LABWEAVER_E2E_RESUME_RUN_ID,
    process.env.LABWEAVER_E2E_RESUME_RELEASE_ID,
  ].some((value) => typeof value === 'string' && value.trim() !== '')
  if (resumeConfigured) return null
  if (process.env.LABWEAVER_E2E_REAL_PROVIDER !== '1') return null
  const model = process.env.LABWEAVER_E2E_PROVIDER_MODEL?.trim() ?? ''
  if (!model || /\s/.test(model)) throw new Error('LABWEAVER_E2E_PROVIDER_MODEL_REQUIRED')
  const base = requireDigestPinnedImage(process.env.LABWEAVER_E2E_SECURITY_BASE_IMAGE)
  return Object.freeze({ model, goldenBaseImage: base.image, goldenBaseDigest: base.digest })
}

/**
 * Resolve an explicit continuation of a completed Work authoring run.
 *
 * All three IDs are required together. The continuation never lists or picks
 * a latest run/release; the validation helper below reads exactly these
 * project, AgentRun, and release identities before starting the resource and
 * Work journey.
 */
export function realWorkResumeConfig() {
  const projectId = process.env.LABWEAVER_E2E_RESUME_PROJECT_ID?.trim() ?? ''
  const runId = process.env.LABWEAVER_E2E_RESUME_RUN_ID?.trim() ?? ''
  const releaseId = process.env.LABWEAVER_E2E_RESUME_RELEASE_ID?.trim() ?? ''
  if (!projectId && !runId && !releaseId) return null
  if (!projectId || !runId || !releaseId) {
    throw new Error('LABWEAVER_E2E_RESUME_WORK_PROJECT_RUN_RELEASE_REQUIRED')
  }
  return Object.freeze({
    projectId,
    runId,
    releaseId,
    // The public ProblemPackage projection exposes immutable object identities;
    // callers supply the exact original markers and resume never invents them.
    seedMarker: process.env.LABWEAVER_E2E_RESUME_SEED_MARKER?.trim() || null,
    persistenceMarker: process.env.LABWEAVER_E2E_RESUME_PERSISTENCE_MARKER?.trim() || null,
  })
}

/**
 * Build a transient Work package with an explicit generated container recipe.
 * The package is intentionally separate from the checked-in experiment fixture:
 * the real provider path needs a Work class, a writable seeded workspace, and a
 * stable marker that can be read again after a Work restart.
 */
export async function createRealWorkPackage(goldenBaseImage) {
  const base = requireDigestPinnedImage(goldenBaseImage)
  const directory = await mkdtemp(join(tmpdir(), 'labweaver-real-work-'))
  const seedMarker = `labweaver-real-work-seed-${uuidv7()}`
  const persistenceMarker = `labweaver-real-work-persistence-${uuidv7()}`
  const dockerfile = [
    `FROM ${base.image}`,
    'COPY seed.txt /opt/labweaver/workspace-seed/seed.txt',
    'USER 0',
    'RUN mkdir -p /workspace /tmp /opt/labweaver/workspace-seed && chmod -R a+rX /opt/labweaver/workspace-seed && chmod 0777 /workspace /tmp',
    'USER 65534:65534',
    'WORKDIR /workspace',
    'CMD ["python3", "-m", "http.server", "8080", "--bind", "0.0.0.0", "--directory", "/workspace"]',
    '',
  ].join('\n')
  const environmentSpec = {
    apiVersion: 'environment.labweaver.io/v1',
    kind: 'EnvironmentSpec',
    name: 'labweaver-real-work',
    class: 'work',
    resources: {
      cpuMillicores: 1000,
      memoryBytes: 2 * 1024 * 1024 * 1024,
      storageBytes: 10 * 1024 * 1024 * 1024,
    },
    network: { mode: 'deny_all' },
    entries: [{ name: 'workspace-files', protocol: 'http', servicePort: 8080 }],
    security: {
      userPolicy: 'non_root_required',
      rootFilesystemPolicy: 'read_only_required',
      privilegeEscalationPolicy: 'deny',
      publicExposurePolicy: 'deny',
      securityProfileBinding: 'restricted-v1',
    },
    runtime: {
      kind: 'container',
      provider_binding: 'kubernetes-work-local-hostpath',
      build_recipe: {
        mode: 'generated',
        files: [
          { path: 'Dockerfile', content: dockerfile },
          { path: 'seed.txt', content: `${seedMarker}\n` },
        ],
      },
      service_port: 8080,
    },
    retention: {
      policyId: uuidv7(),
      policyRevision: 1,
      class: 'run_evidence',
      retainUntil: new Date(Date.now() + 24 * 60 * 60 * 1000).toISOString(),
      disposition: 'delete',
    },
  }
  const readme = [
    '# Real Work provider package',
    '',
    'This package is used only by the explicitly opted-in real provider E2E path.',
    'Return the nested environmentSpec exactly, adapting only the generated container build recipe.',
    'The Work must remain class=work and use the kubernetes-work-local-hostpath provider binding.',
    `The generated Dockerfile must start FROM ${base.image}, copy seed.txt into /opt/labweaver/workspace-seed/seed.txt, and run the Python HTTP service on port 8080 as UID/GID 65534.`,
    `The initial workspace must expose the exact seed marker ${seedMarker} through the HTTP endpoint.`,
    '',
  ].join('\n')
  const configurationInstructions = [
    '# Work configuration instructions',
    '',
    'For the WorkConfiguration request, write the exact persistence marker below to /workspace/persistence-marker.txt.',
    `Persistence marker: ${persistenceMarker}`,
    'Use a complete executable POSIX shell script and a separate verification script.',
    'The verification script must fail if /workspace/persistence-marker.txt is missing or has another value.',
    'This file-only change does not require a Work restart.',
    '',
  ].join('\n')

  await writeFile(join(directory, 'README.md'), readme, 'utf8')
  await writeFile(join(directory, 'environment-spec.json'), `${JSON.stringify({ environmentSpec }, null, 2)}\n`, 'utf8')
  await writeFile(join(directory, 'work-configuration.md'), configurationInstructions, 'utf8')

  return {
    directory,
    seedMarker,
    persistenceMarker,
    environmentSpec,
    dockerfile,
    async cleanup() {
      await rm(directory, { recursive: true, force: true })
    },
  }
}

function sameContainerArtifact(actual, expected, code) {
  const actualBuildRequestId = actual?.build_request_id ?? actual?.buildRequestId
  const expectedBuildRequestId = expected?.build_request_id ?? expected?.buildRequestId
  if (
    actual?.kind !== 'container'
    || expected?.kind !== 'container'
    || actual.id !== expected.id
    || actual.repository !== expected.repository
    || actual.digest?.toLowerCase() !== expected.digest?.toLowerCase()
    || actualBuildRequestId !== expectedBuildRequestId
  ) {
    throw new Error(code)
  }
  return actual
}

function resumableContainerArtifact(candidate) {
  const artifact = candidate?.imageArtifact ?? candidate?.build?.artifact
  if (!artifact || artifact.kind !== 'container') throw new Error('REAL_WORK_RESUME_CONTAINER_ARTIFACT_MISSING')
  if (typeof artifact.repository !== 'string' || artifact.repository.trim() === '') {
    throw new Error('REAL_WORK_RESUME_CONTAINER_REPOSITORY_MISSING')
  }
  if (!/^sha256:[0-9a-f]{64}$/i.test(artifact.digest ?? '')) {
    throw new Error('REAL_WORK_RESUME_CONTAINER_DIGEST_INVALID')
  }
  return artifact
}

async function readResumeActorId(request) {
  const session = await expectJson(await request.get('/api/v1/auth/session'), 'REAL_WORK_RESUME_SESSION_READ_FAILED')
  const actorId = session.actor?.actorId
  if (typeof actorId !== 'string' || actorId.length === 0) throw new Error('REAL_WORK_RESUME_ACTOR_ID_MISSING')
  return actorId
}

/**
 * Validate a previously completed Work authoring/build/publication chain by
 * its exact IDs. This read-only gate is the only entry point for resume mode.
 */
export async function readResumablePublishedWork(request, resume) {
  const { projectId, runId, releaseId } = resume
  const actorId = await readResumeActorId(request)
  const project = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}`),
    'REAL_WORK_RESUME_PROJECT_READ_FAILED',
  )
  if (project.id !== projectId || project.ownerActorId !== actorId) {
    throw new Error('REAL_WORK_RESUME_PROJECT_OWNERSHIP_INVALID')
  }

  const run = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/agent-runs/${encodeURIComponent(runId)}`),
    'REAL_WORK_RESUME_AGENT_RUN_READ_FAILED',
  )
  const environmentTrack = run.tracks?.find((track) => track.kind === 'environment')
  if (
    run.id !== runId
    || run.projectId !== projectId
    || run.state !== 'succeeded'
    || run.purpose?.kind !== 'authoring'
    || run.purpose.environmentClass !== 'work'
    || !Array.isArray(run.tracks)
    || run.tracks.length !== 1
    || !environmentTrack?.candidateId
    || typeof run.packageId !== 'string'
  ) {
    throw new Error('REAL_WORK_RESUME_AGENT_RUN_INVALID')
  }

  const candidateView = await expectJson(
    await request.get(
      `/api/v1/projects/${encodeURIComponent(projectId)}/environment-candidates/${encodeURIComponent(environmentTrack.candidateId)}`,
    ),
    'REAL_WORK_RESUME_ENVIRONMENT_CANDIDATE_READ_FAILED',
  )
  const candidate = candidateView.candidate
  if (
    candidate?.id !== environmentTrack.candidateId
    || candidate.runId !== runId
    || candidate.projectId !== projectId
    || candidate.spec?.class !== 'work'
    || candidateView.build?.state !== 'succeeded'
    || candidateView.imageArtifact == null
  ) {
    throw new Error('REAL_WORK_RESUME_CANDIDATE_INVALID')
  }
  const builtArtifact = resumableContainerArtifact(candidateView)
  sameContainerArtifact(builtArtifact, candidateView.build.artifact, 'REAL_WORK_RESUME_BUILD_ARTIFACT_INVALID')
  sameContainerArtifact(builtArtifact, candidateView.imageArtifact, 'REAL_WORK_RESUME_CANDIDATE_ARTIFACT_INVALID')
  const candidateApproval = Array.isArray(candidateView.approvals)
    ? candidateView.approvals.find((item) => (
      item.candidateId === candidate.id
      && item.candidateRevision === candidate.revision
      && item.policyRevision === candidate.policyRevision
      && item.trustRevision === candidateView.trustRevision
      && item.decision === 'approved'
    ))
    : undefined
  if (!candidateApproval) throw new Error('REAL_WORK_RESUME_CANDIDATE_NOT_APPROVED')

  const release = await expectJson(
    await request.get(
      `/api/v1/projects/${encodeURIComponent(projectId)}/environment-template-releases/${encodeURIComponent(releaseId)}`,
    ),
    'REAL_WORK_RESUME_RELEASE_READ_FAILED',
  )
  if (
    release.id !== releaseId
    || release.projectId !== projectId
    || release.agentRunId !== runId
    || release.candidateId !== candidate.id
    || release.candidateRevision !== candidate.revision
    || !Number.isInteger(release.version)
    || release.version < 1
    || release.runtimeKind !== 'container'
    || release.approval?.id !== candidateApproval.id
    || release.approval?.candidateId !== candidate.id
    || release.approval?.candidateRevision !== candidate.revision
    || release.approval?.policyRevision !== candidateApproval.policyRevision
    || release.approval?.trustRevision !== candidateApproval.trustRevision
    || release.approval?.decision !== 'approved'
  ) {
    throw new Error('REAL_WORK_RESUME_RELEASE_BINDING_INVALID')
  }
  sameContainerArtifact(release.artifact, builtArtifact, 'REAL_WORK_RESUME_RELEASE_ARTIFACT_INVALID')

  const packageData = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/problem-packages/${encodeURIComponent(run.packageId)}`),
    'REAL_WORK_RESUME_PACKAGE_READ_FAILED',
  )
  if (packageData.id !== run.packageId || packageData.projectId !== projectId || !Number.isInteger(packageData.revision) || packageData.revision < 1) {
    throw new Error('REAL_WORK_RESUME_PACKAGE_INVALID')
  }

  if (!resume.seedMarker || !resume.persistenceMarker) {
    throw new Error('REAL_WORK_RESUME_MARKERS_REQUIRED')
  }

  return {
    project,
    run,
    candidateView,
    release,
    packageData,
    seedMarker: resume.seedMarker,
    persistenceMarker: resume.persistenceMarker,
  }
}

function isCurrentPositiveUsdRate(rate, unit, now) {
  return rate?.unit === unit
    && rate?.gpuClass == null
    && rate?.gpuMode == null
    && rate?.unitQuantity > 0
    && rate?.unitPrice?.currency === 'USD'
    && Number(rate.unitPrice.amount) > 0
    && Number.isFinite(Date.parse(rate.effectiveFrom))
    && Date.parse(rate.effectiveFrom) <= now
    && (rate.effectiveUntil == null || Date.parse(rate.effectiveUntil) > now)
}

/**
 * Ensure CPU, memory, and storage have real non-zero USD rates before usage is
 * measured. Rate creation is intentionally through the admin API because the
 * admin finance page exposes budgets and charges, not the immutable rate card.
 */
export async function ensureRealWorkRates(browser, baseURL) {
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  try {
    const ratesResponse = await context.request.get('/api/v1/resource/rates')
    const existing = await expectJson(ratesResponse, 'REAL_WORK_RATES_LIST_FAILED')
    if (!Array.isArray(existing)) throw new Error('REAL_WORK_RATES_RESPONSE_INVALID')
    const now = Date.now()
    const rates = []
    for (const unit of BILLING_UNITS) {
      const current = existing.find((rate) => isCurrentPositiveUsdRate(rate, unit, now))
      if (current) {
        rates.push(current)
        continue
      }
      const input = {
        unit,
        unitQuantity: 1_000_000,
        gpuClass: null,
        gpuMode: null,
        unitPrice: { currency: 'USD', amount: '1.000000' },
        effectiveFrom: new Date(now - 120_000).toISOString(),
        effectiveUntil: null,
      }
      const response = await context.request.post('/api/v1/resource/rates', {
        headers: await csrfHeaders(context.request, baseURL, { 'Idempotency-Key': uuidv7() }),
        data: input,
      })
      const rate = await expectJson(response, `REAL_WORK_${unit.toUpperCase()}_RATE_CREATE_FAILED`)
      if (!isCurrentPositiveUsdRate(rate, unit, Date.now())) throw new Error(`REAL_WORK_RATE_INVALID:${unit}`)
      rates.push(rate)
    }
    return rates
  } finally {
    await context.close()
  }
}

export async function configureRealWorkBudgetByUi(browser, baseURL, projectId) {
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  const page = await context.newPage()
  try {
    await page.goto('/admin/resource-finance', { waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('heading', { name: '预算与费用', exact: true })).toBeVisible({ timeout: 120_000 })
    const projectSelect = page.locator('.project-strip select')
    await expect(projectSelect.locator(`option[value="${projectId}"]`)).toHaveCount(1, { timeout: 120_000 })
    await projectSelect.selectOption(projectId)
    await expect(page.locator('.budget-form')).toBeVisible({ timeout: 120_000 })
    const inputs = page.locator('.budget-form input')
    await expect(inputs).toHaveCount(3)
    const currency = inputs.nth(0)
    if (!(await currency.isEditable())) {
      await expect(currency).toHaveValue('USD')
    } else {
      await currency.fill('USD')
    }
    await inputs.nth(1).fill('1000000000.000000')
    await inputs.nth(2).fill('900000000.000000')
    const responsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'PUT'
        && url.pathname === `/api/v1/projects/${projectId}/resource-budget`
    })
    await page.locator('.budget-form button[type="submit"]').click()
    await expectJson(await responsePromise, 'REAL_WORK_RESOURCE_BUDGET_SAVE_FAILED')
    await expect(page.locator('.budget-summary')).toBeVisible({ timeout: 120_000 })
  } finally {
    await context.close()
  }
}

export function assertRealWorkCharges(charges) {
  if (!Array.isArray(charges)) throw new Error('REAL_WORK_CHARGES_RESPONSE_INVALID')
  const positiveSettled = charges.filter((charge) => charge.settlement === 'settled' && Number(charge.total?.amount) > 0)
  const units = new Set(positiveSettled.flatMap((charge) => (charge.lines ?? []).filter((line) => Number(line.amount?.amount) > 0).map((line) => line.unit)))
  for (const unit of BILLING_UNITS) {
    if (!units.has(unit)) throw new Error(`REAL_WORK_POSITIVE_SETTLED_CHARGE_MISSING:${unit}`)
  }
  return positiveSettled
}

export async function waitForRealWorkCharges(browser, baseURL, projectId) {
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  try {
    let latest
    await expect.poll(async () => {
      const response = await context.request.get(`/api/v1/projects/${projectId}/charges`)
      latest = await expectJson(response, 'REAL_WORK_CHARGES_READ_FAILED')
      try {
        assertRealWorkCharges(latest)
        return true
      } catch {
        return false
      }
    }, { timeout: 300_000, intervals: [1000, 2000, 3000] }).toBe(true)
    assertRealWorkCharges(latest)
    const budget = await expectJson(
      await context.request.get(`/api/v1/projects/${projectId}/resource-budget`),
      'REAL_WORK_BUDGET_READ_FAILED',
    )
    if (budget.limit?.currency !== 'USD' || Number(budget.spent?.amount) <= 0) {
      throw new Error('REAL_WORK_BUDGET_SPENT_NOT_POSITIVE')
    }
    return { charges: latest, budget }
  } finally {
    await context.close()
  }
}

export async function inspectRealWorkFinanceByUi(browser, baseURL, projectId) {
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  const page = await context.newPage()
  try {
    await page.goto('/admin/resource-finance', { waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('heading', { name: '预算与费用', exact: true })).toBeVisible({ timeout: 120_000 })
    const projectSelect = page.locator('.project-strip select')
    await expect(projectSelect.locator(`option[value="${projectId}"]`)).toHaveCount(1, { timeout: 120_000 })
    await projectSelect.selectOption(projectId)
    await expect(page.locator('.charge-row').first()).toBeVisible({ timeout: 120_000 })
    const spent = page
      .locator('.budget-summary > div')
      .filter({ hasText: '已花费' })
      .locator('strong')
    await expect(spent).toHaveCount(1, { timeout: 120_000 })
    await expect.poll(
      async () => {
        const text = (await spent.textContent())?.trim() ?? ''
        const [amount, currency] = text.split(/\s+/)
        return currency === 'USD'
          && /^\d+\.\d{6}$/.test(amount ?? '')
          && /[1-9]/.test((amount ?? '').replace('.', ''))
      },
      { timeout: 120_000, intervals: [500, 1000, 2000] },
    ).toBe(true)
  } finally {
    await context.close()
  }
}
