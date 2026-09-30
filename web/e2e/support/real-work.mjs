import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect } from '@playwright/test'
import {
  AUTH_STATE,
  expectJson,
  uuidv7,
} from './live.mjs'

const DIGEST_PINNED_IMAGE = /^[^\s@]+@sha256:([0-9a-f]{64})$/i
const BILLING_UNITS = Object.freeze([
  'cpu_millicore_second',
  'memory_byte_second',
  'storage_byte_second',
])
const DEFAULT_RATE_INPUTS = Object.freeze([
  Object.freeze({ unit: 'cpu_millicore_second', unitQuantity: 1_000_000, amount: '1.000000', currency: 'USD' }),
  Object.freeze({ unit: 'memory_byte_second', unitQuantity: 1_000_000, amount: '1.000000', currency: 'USD' }),
  Object.freeze({ unit: 'storage_byte_second', unitQuantity: 1_000_000, amount: '1.000000', currency: 'USD' }),
])
const GPU_MODES = Object.freeze(['exclusive', 'container_time_slice', 'vm_vgpu'])
const FIXED_DECIMAL = /^(0|[1-9][0-9]*)\.[0-9]{6}$/
const GPU_CLASS = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/
const CURRENCY = /^[A-Za-z0-9_-]{1,32}$/
const VM_BINDING = /^[a-z0-9](?:[a-z0-9._-]{0,126}[a-z0-9])?$/
const VM_SOURCE_REGISTRY_DIGEST = /^docker:\/\/[^\s@]+@sha256:[0-9a-f]{64}$/i
const VM_DISK_FORMATS = Object.freeze(['qcow2', 'raw'])

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
 * Resolve the explicit GPU request for the Work resource leg.
 *
 * A normal Work run remains CPU-only unless all four GPU inputs are supplied.
 * The rate amount and currency are used only when the admin UI has to create
 * the matching GPU rate; the catalog class and mode are still selected from
 * the Resource page's published catalog options.
 */
export function realWorkGpuConfig() {
  const gpuClass = process.env.LABWEAVER_E2E_WORK_GPU_CLASS?.trim() ?? ''
  const gpuMode = process.env.LABWEAVER_E2E_WORK_GPU_MODE?.trim() ?? ''
  const rateAmount = process.env.LABWEAVER_E2E_WORK_GPU_RATE_AMOUNT?.trim() ?? ''
  const rateCurrency = process.env.LABWEAVER_E2E_WORK_GPU_RATE_CURRENCY?.trim() ?? ''
  const present = [gpuClass, gpuMode, rateAmount, rateCurrency].filter((value) => value !== '')
  if (present.length === 0) return null
  if (present.length !== 4) throw new Error('LABWEAVER_E2E_WORK_GPU_FIELDS_INCOMPLETE')
  if (!GPU_CLASS.test(gpuClass)) throw new Error('LABWEAVER_E2E_WORK_GPU_CLASS_INVALID')
  if (!GPU_MODES.includes(gpuMode)) throw new Error('LABWEAVER_E2E_WORK_GPU_MODE_INVALID')
  if (!FIXED_DECIMAL.test(rateAmount) || Number(rateAmount) <= 0) {
    throw new Error('LABWEAVER_E2E_WORK_GPU_RATE_AMOUNT_INVALID')
  }
  if (!CURRENCY.test(rateCurrency)) throw new Error('LABWEAVER_E2E_WORK_GPU_RATE_CURRENCY_INVALID')
  return Object.freeze({
    class: gpuClass,
    mode: gpuMode,
    count: 1,
    rate: Object.freeze({
      unit: 'gpu_unit_second',
      unitQuantity: 1,
      amount: rateAmount,
      currency: rateCurrency,
      gpuClass,
      gpuMode,
    }),
  })
}

/**
 * Resolve the explicit, deployment-reviewed VM release identity used by the
 * resume path. VM Work authoring is intentionally not inferred from a
 * container image setting: the caller must provide every base-disk and
 * provider binding that the already-approved release is expected to carry.
 */
export function realWorkVmConfig() {
  const providerBinding = process.env.LABWEAVER_E2E_VM_PROVIDER_BINDING?.trim() ?? ''
  const storageClassBinding = process.env.LABWEAVER_E2E_VM_STORAGE_CLASS_BINDING?.trim() ?? ''
  const baseDiskBinding = process.env.LABWEAVER_E2E_VM_BASE_DISK_BINDING?.trim() ?? ''
  const sourceRegistryDigest = process.env.LABWEAVER_E2E_VM_BASE_DISK_SOURCE_REGISTRY_DIGEST?.trim() ?? ''
  const capacityBytes = process.env.LABWEAVER_E2E_VM_BASE_DISK_CAPACITY_BYTES?.trim() ?? ''
  const sshPort = process.env.LABWEAVER_E2E_VM_SSH_PORT?.trim() ?? ''
  const values = [providerBinding, storageClassBinding, baseDiskBinding, sourceRegistryDigest, capacityBytes, sshPort]
  if (values.every((value) => value === '')) return null
  if (values.some((value) => value === '')) throw new Error('LABWEAVER_E2E_VM_FIELDS_INCOMPLETE')
  if (!VM_BINDING.test(providerBinding)) throw new Error('LABWEAVER_E2E_VM_PROVIDER_BINDING_INVALID')
  if (!VM_BINDING.test(storageClassBinding)) throw new Error('LABWEAVER_E2E_VM_STORAGE_CLASS_BINDING_INVALID')
  if (!VM_BINDING.test(baseDiskBinding)) throw new Error('LABWEAVER_E2E_VM_BASE_DISK_BINDING_INVALID')
  if (!VM_SOURCE_REGISTRY_DIGEST.test(sourceRegistryDigest)) throw new Error('LABWEAVER_E2E_VM_BASE_DISK_DIGEST_INVALID')
  if (!/^[1-9][0-9]{8,15}$/.test(capacityBytes)) throw new Error('LABWEAVER_E2E_VM_BASE_DISK_CAPACITY_INVALID')
  if (sshPort !== '22') throw new Error('LABWEAVER_E2E_VM_SSH_PORT_INVALID')
  return Object.freeze({
    providerBinding,
    storageClassBinding,
    baseDisk: Object.freeze({
      binding: baseDiskBinding,
      sourceRegistryDigest,
      capacityBytes: Number(capacityBytes),
    }),
    sshPort: Number(sshPort),
  })
}

/**
 * Build a transient Work package with an explicit generated container recipe.
 * The package is intentionally separate from the checked-in experiment fixture:
 * the real provider path needs a Work class, a writable seeded workspace, and a
 * stable marker that can be read again after a Work restart.
 */
export async function createRealWorkPackage(
  goldenBaseImage,
  { gpu = null, providerBinding = 'kubernetes-work-local-hostpath' } = {},
) {
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
      ...(gpu ? { gpu: { class: gpu.class, count: gpu.count } } : {}),
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
      provider_binding: providerBinding,
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
    `The Work must remain class=work and use the ${providerBinding} provider binding.`,
    `The generated Dockerfile must start FROM ${base.image}, copy seed.txt into /opt/labweaver/workspace-seed/seed.txt, and run the Python HTTP service on port 8080 as UID/GID 65534.`,
    `The initial workspace must expose the exact seed marker ${seedMarker} through the HTTP endpoint.`,
    ...(gpu ? [`The Work resource request must include GPU class ${gpu.class} with count ${gpu.count}.`] : []),
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

function sameVirtualMachineArtifact(actual, expected, code) {
  if (
    actual?.kind !== 'virtual_machine'
    || expected?.kind !== 'virtual_machine'
    || actual.id !== expected.id
    || actual.format !== expected.format
    || actual.base_disk?.binding !== expected.base_disk?.binding
    || actual.base_disk?.capacityBytes !== expected.base_disk?.capacityBytes
    || actual.base_disk?.sourceRegistryDigest?.toLowerCase() !== expected.base_disk?.sourceRegistryDigest?.toLowerCase()
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
export async function readResumablePublishedWork(request, resume, { gpu = null, vm = null } = {}) {
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
    || candidateView.imageArtifact == null
  ) {
    throw new Error('REAL_WORK_RESUME_CANDIDATE_INVALID')
  }
  const runtime = candidate.spec.runtime
  const candidateBuildReady = runtime?.kind === 'virtual_machine'
    ? candidateView.build == null || candidateView.build.state === 'succeeded'
    : candidateView.build?.state === 'succeeded'
  if (!candidateBuildReady) throw new Error('REAL_WORK_RESUME_CANDIDATE_BUILD_INVALID')
  if (gpu) {
    const declaredGpu = candidate.spec?.resources?.gpu
    if (declaredGpu?.class !== gpu.class || declaredGpu?.count !== gpu.count) {
      throw new Error('REAL_WORK_RESUME_GPU_SPEC_MISMATCH')
    }
  }
  if ((runtime?.kind === 'virtual_machine') !== Boolean(vm)) {
    throw new Error('REAL_WORK_RESUME_RUNTIME_CONFIGURATION_MISMATCH')
  }
  const runtimeArtifact = vm
    ? candidateView.imageArtifact
    : resumableContainerArtifact(candidateView)
  if (vm) {
    if (
      runtime?.provider_binding !== vm.providerBinding
      || runtime.storage_class_binding !== vm.storageClassBinding
      || runtime.ssh_port !== vm.sshPort
      || runtime.base_disk?.binding !== vm.baseDisk.binding
      || runtime.base_disk?.capacityBytes !== vm.baseDisk.capacityBytes
      || runtime.base_disk?.sourceRegistryDigest?.toLowerCase() !== vm.baseDisk.sourceRegistryDigest.toLowerCase()
      || runtimeArtifact?.kind !== 'virtual_machine'
      || runtimeArtifact.base_disk?.binding !== vm.baseDisk.binding
      || runtimeArtifact.base_disk?.capacityBytes !== vm.baseDisk.capacityBytes
      || runtimeArtifact.base_disk?.sourceRegistryDigest?.toLowerCase() !== vm.baseDisk.sourceRegistryDigest.toLowerCase()
    ) {
      throw new Error('REAL_WORK_RESUME_VM_BINDING_INVALID')
    }
    if (!VM_DISK_FORMATS.includes(runtimeArtifact.format)) throw new Error('REAL_WORK_RESUME_VM_ARTIFACT_FORMAT_INVALID')
  } else {
    sameContainerArtifact(runtimeArtifact, candidateView.build.artifact, 'REAL_WORK_RESUME_BUILD_ARTIFACT_INVALID')
    sameContainerArtifact(runtimeArtifact, candidateView.imageArtifact, 'REAL_WORK_RESUME_CANDIDATE_ARTIFACT_INVALID')
  }
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
    || release.runtimeKind !== (vm ? 'virtual_machine' : 'container')
    || release.approval?.id !== candidateApproval.id
    || release.approval?.candidateId !== candidate.id
    || release.approval?.candidateRevision !== candidate.revision
    || release.approval?.policyRevision !== candidateApproval.policyRevision
    || release.approval?.trustRevision !== candidateApproval.trustRevision
    || release.approval?.decision !== 'approved'
  ) {
    throw new Error('REAL_WORK_RESUME_RELEASE_BINDING_INVALID')
  }
  if (vm) sameVirtualMachineArtifact(release.artifact, runtimeArtifact, 'REAL_WORK_RESUME_RELEASE_ARTIFACT_INVALID')
  else sameContainerArtifact(release.artifact, runtimeArtifact, 'REAL_WORK_RESUME_RELEASE_ARTIFACT_INVALID')

  const packageData = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(projectId)}/problem-packages/${encodeURIComponent(run.packageId)}`),
    'REAL_WORK_RESUME_PACKAGE_READ_FAILED',
  )
  if (packageData.id !== run.packageId || packageData.projectId !== projectId || !Number.isInteger(packageData.revision) || packageData.revision < 1) {
    throw new Error('REAL_WORK_RESUME_PACKAGE_INVALID')
  }

  if ((!vm && !resume.seedMarker) || !resume.persistenceMarker) {
    throw new Error(vm ? 'REAL_WORK_RESUME_VM_PERSISTENCE_MARKER_REQUIRED' : 'REAL_WORK_RESUME_MARKERS_REQUIRED')
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

function currentRateDimension(rate, target, now = Date.now()) {
  const effectiveFrom = Date.parse(rate?.effectiveFrom)
  const effectiveUntil = rate?.effectiveUntil == null ? Number.POSITIVE_INFINITY : Date.parse(rate.effectiveUntil)
  if (
    !rate
    || rate.unit !== target.unit
    || !Number.isSafeInteger(rate.unitQuantity)
    || rate.unitQuantity < 1
    || !Number.isFinite(effectiveFrom)
    || effectiveFrom > now
    || !Number.isFinite(effectiveUntil)
    || effectiveUntil <= now
  ) return false
  if (target.unit === 'gpu_unit_second') {
    return rate.gpuClass === target.gpuClass && rate.gpuMode === target.gpuMode
  }
  return rate.gpuClass == null && rate.gpuMode == null
}

function rateMatchesInput(rate, target) {
  return currentRateDimension(rate, target)
    && rate.unitQuantity === target.unitQuantity
    && rate.unitPrice?.currency === target.currency
    && rate.unitPrice?.amount === target.amount
}

function hasPositiveRatePrice(rate) {
  return typeof rate?.unitPrice?.currency === 'string'
    && rate.unitPrice.currency.length > 0
    && FIXED_DECIMAL.test(rate.unitPrice.amount ?? '')
    && Number(rate.unitPrice.amount) > 0
}

async function readResourceRates(context, diagnosticCode) {
  const rates = await expectJson(
    await context.request.get('/api/v1/resource/rates'),
    diagnosticCode,
  )
  if (!Array.isArray(rates)) throw new Error('REAL_WORK_RATES_RESPONSE_INVALID')
  return rates
}

function localDateTimeValue(date = new Date()) {
  const pad = (value) => String(value).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

async function waitForRateListUi(page, unit) {
  const form = page.getByTestId('resource-rate-form')
  await expect(form).toBeVisible({ timeout: 120_000 })
  await expect.poll(
    async () => {
      const errorBanner = page.locator('section.rates-card [role="alert"]')
      if (await errorBanner.count()) {
        const code = (await errorBanner.first().locator('.diagnostic-code').textContent())?.trim() || 'diagnostic-missing'
        throw new Error(`REAL_WORK_${unit.toUpperCase()}_RATES_LOAD_FAILED:${code}`)
      }
      return (await page.locator('section.rates-card ul[aria-label="资源费率列表"]').count()) > 0
        || (await page.getByText('还没有资源费率。创建费率后，匹配的 GPU 目录项才能用于资源申请。', { exact: true }).count()) > 0
    },
    { timeout: 120_000, intervals: [250, 500, 1000] },
  ).toBe(true)
}

async function readRateRowsFromUi(page) {
  const list = page.locator('section.rates-card ul[aria-label="资源费率列表"]')
  if (await list.count() === 0) return []
  return list.locator('li.rate-row').evaluateAll((rows) => rows.map((row) => ({
    label: row.querySelector('.rate-main strong')?.textContent?.trim() || '',
    detail: row.querySelector('.rate-main small')?.textContent?.trim() || '',
    revision: Number(row.querySelector('.state-chip')?.textContent?.match(/版本\s+(\d+)/)?.[1] || NaN),
  })))
}

async function createRateByUi(page, target) {
  const form = page.getByTestId('resource-rate-form')
  await form.locator('select').first().selectOption(target.unit)
  if (target.unit === 'gpu_unit_second') {
    await form.getByLabel('GPU class', { exact: true }).fill(target.gpuClass)
    await form.getByLabel('分配模式', { exact: true }).selectOption(target.gpuMode)
  }
  await form.getByLabel('每次计费基础单位数', { exact: true }).fill(String(target.unitQuantity))
  await form.getByLabel('单价', { exact: true }).fill(target.amount)
  await form.getByLabel('币种', { exact: true }).fill(target.currency)
  await form.getByLabel('生效时间', { exact: true }).fill(localDateTimeValue(new Date(Date.now() - 120_000)))
  await form.getByLabel('结束时间（可选）', { exact: true }).fill('')

  const createButton = form.getByRole('button', { name: '创建费率版本', exact: true })
  await expect(createButton).toBeEnabled()
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST' && url.pathname === '/api/v1/resource/rates'
  })
  await createButton.click()
  const response = await responsePromise
  if (!response.ok()) throw new Error(`REAL_WORK_${target.unit.toUpperCase()}_RATE_CREATE_FAILED:status-${response.status()}`)
  await expect(page.locator('.diagnostic-banner').filter({ hasText: '资源费率已创建。' })).toBeVisible({ timeout: 120_000 })
}

async function waitForRateUiReadback(page, target, revision) {
  const expectedLabel = target.unit === 'gpu_unit_second'
    ? `GPU ${target.gpuClass} · ${({ exclusive: '独占', container_time_slice: '容器时间片', vm_vgpu: 'VM vGPU' })[target.gpuMode]}`
    : ({ cpu_millicore_second: 'CPU', memory_byte_second: '内存', storage_byte_second: '存储' })[target.unit]
  await expect.poll(async () => {
    const rows = await readRateRowsFromUi(page)
    return rows.some((row) => (
      row.label === expectedLabel
      && row.detail.startsWith(`${target.unitQuantity} 基础单位 · ${target.amount} ${target.currency} ·`)
      && row.revision === revision
    ))
  }, { timeout: 120_000, intervals: [250, 500, 1000] }).toBe(true)
}

async function ensureRateByUi(page, context, target) {
  let rates = await readResourceRates(context, 'REAL_WORK_RATES_LIST_FAILED')
  let current = rates.filter((rate) => currentRateDimension(rate, target))
  if (current.length > 1) throw new Error(`REAL_WORK_RATE_ACTIVE_AMBIGUOUS:${target.unit}`)
  if (current.length === 1) {
    if (!hasPositiveRatePrice(current[0])) throw new Error(`REAL_WORK_RATE_ACTIVE_INVALID:${target.unit}`)
    if (target.unit === 'gpu_unit_second' && !rateMatchesInput(current[0], target)) {
      throw new Error(`REAL_WORK_GPU_RATE_ACTIVE_CONFLICT:${target.gpuClass}`)
    }
    return current[0]
  }

  await createRateByUi(page, target)
  rates = await readResourceRates(context, 'REAL_WORK_RATES_READBACK_FAILED')
  current = rates.filter((rate) => currentRateDimension(rate, target))
  if (current.length !== 1) throw new Error(`REAL_WORK_RATE_ACTIVE_READBACK_INVALID:${target.unit}`)
  if (!rateMatchesInput(current[0], target)) {
    throw new Error(`REAL_WORK_RATE_ACTIVE_READBACK_MISMATCH:${target.unit}`)
  }
  await waitForRateUiReadback(page, target, current[0].revision)
  return current[0]
}

/**
 * Ensure the Work dimensions have effective rates through the administrator's
 * finance form. Existing active CPU, memory, and storage rates are reused at
 * their operator-selected values. An explicitly requested GPU dimension must
 * match its requested amount and currency, otherwise the run fails closed
 * instead of replacing a global rate used by other projects.
 */
export async function ensureRealWorkRates(browser, baseURL, { gpu = null } = {}) {
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  const page = await context.newPage()
  try {
    await page.goto('/admin/resource-finance', { waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('heading', { name: '预算与费用', exact: true })).toBeVisible({ timeout: 120_000 })
    await waitForRateListUi(page, 'resource')
    const targets = [...DEFAULT_RATE_INPUTS, ...(gpu ? [gpu.rate] : [])]
    const rates = []
    for (const target of targets) rates.push(await ensureRateByUi(page, context, target))
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

export function assertRealWorkCharges(charges, gpu = null) {
  if (!Array.isArray(charges)) throw new Error('REAL_WORK_CHARGES_RESPONSE_INVALID')
  const positiveSettled = charges.filter((charge) => charge.settlement === 'settled' && Number(charge.total?.amount) > 0)
  const units = new Set(positiveSettled.flatMap((charge) => (charge.lines ?? []).filter((line) => Number(line.amount?.amount) > 0).map((line) => line.unit)))
  for (const unit of BILLING_UNITS) {
    if (!units.has(unit)) throw new Error(`REAL_WORK_POSITIVE_SETTLED_CHARGE_MISSING:${unit}`)
  }
  if (gpu && !units.has('gpu_unit_second')) throw new Error('REAL_WORK_POSITIVE_SETTLED_CHARGE_MISSING:gpu_unit_second')
  return positiveSettled
}

export async function waitForRealWorkCharges(browser, baseURL, projectId, { gpu = null } = {}) {
  const context = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  try {
    let latest
    await expect.poll(async () => {
      const response = await context.request.get(`/api/v1/projects/${projectId}/charges`)
      latest = await expectJson(response, 'REAL_WORK_CHARGES_READ_FAILED')
      try {
        assertRealWorkCharges(latest, gpu)
        return true
      } catch {
        return false
      }
    }, { timeout: 300_000, intervals: [1000, 2000, 3000] }).toBe(true)
    assertRealWorkCharges(latest, gpu)
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

export async function inspectRealWorkFinanceByUi(browser, baseURL, projectId, { gpu = null } = {}) {
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
    if (gpu) {
      const gpuLine = page.locator('.charge-line').filter({ hasText: 'GPU' })
      await expect(gpuLine.first()).toBeVisible({ timeout: 120_000 })
    }
  } finally {
    await context.close()
  }
}
