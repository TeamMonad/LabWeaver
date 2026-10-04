import { readFile, readdir } from 'node:fs/promises'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { expect as playwrightExpect } from '@playwright/test'
import {
  assertRealWorkVmCandidate,
  assertRealWorkGpuContainerCandidate,
  createRealWorkPackage,
  cleanupWorkResources,
  DEFAULT_RATE_INPUTS,
  ensureRateByUi,
  realWorkConfig,
  realWorkVmConfig,
  readResumablePublishedWork,
} from '../e2e/support/real-work.mjs'

const GIB = 1024 ** 3
describe('Work acceptance demonstration rates', () => {
  const now = Date.parse('2026-10-04T06:00:00Z')
  const context = (rates) => ({ request: { get: vi.fn(async () => ({ ok: () => true, text: async () => JSON.stringify(rates) })) } })
  const rate = (target, overrides = {}) => ({
    id: 'operator-rate', revision: 1, unit: target.unit, unitQuantity: target.unitQuantity,
    gpuClass: null, gpuMode: null, unitPrice: { amount: '7.000000', currency: 'USD' },
    effectiveFrom: '2026-10-04T05:00:00Z', effectiveUntil: null, ...overrides,
  })

  beforeEach(() => vi.spyOn(Date, 'now').mockReturnValue(now))
  afterEach(() => vi.restoreAllMocks())

  it.each([
    ['cpu_millicore_second', 1000 * 3600],
    ['memory_byte_second', GIB * 3600],
    ['storage_byte_second', GIB * 3600],
  ])('uses one human resource-hour as the %s demonstration price quantity', (unit, quantity) => {
    const target = DEFAULT_RATE_INPUTS.find((item) => item.unit === unit)
    expect(target).toBeDefined()
    expect(Number.isSafeInteger(target.unitQuantity)).toBe(true)
    expect(quantity / target.unitQuantity * Number(target.amount)).toBe(1)
    expect(target.currency).toBe('USD')
  })

  it('keeps the current operator price when a later version is scheduled', async () => {
    const target = DEFAULT_RATE_INPUTS[1]
    const current = rate(target, { unitQuantity: 1_000_000, effectiveUntil: '2026-10-05T00:00:00Z' })
    const future = rate(target, { id: 'future', revision: 2, effectiveFrom: current.effectiveUntil })
    const page = { getByTestId: vi.fn() }
    await expect(ensureRateByUi(page, context([future, current]), target)).resolves.toEqual(current)
    expect(page.getByTestId).not.toHaveBeenCalled()
  })

  it('does not insert a backdated demonstration price before a scheduled operator rate', async () => {
    const target = DEFAULT_RATE_INPUTS[0]
    const page = { getByTestId: vi.fn() }
    await expect(ensureRateByUi(page, context([rate(target, { effectiveFrom: '2026-10-05T00:00:00Z' })]), target))
      .rejects.toThrow('REAL_WORK_RATE_FUTURE_CONFIGURED:cpu_millicore_second')
    expect(page.getByTestId).not.toHaveBeenCalled()
  })

  it.each([0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1])('rejects a configured unsafe rate quantity %s before a create action', async (quantity) => {
    const target = DEFAULT_RATE_INPUTS[0]
    const page = { getByTestId: vi.fn() }
    await expect(ensureRateByUi(page, context([rate(target, { unitQuantity: quantity })]), target))
      .rejects.toThrow('REAL_WORK_RATE_QUANTITY_INVALID')
    expect(page.getByTestId).not.toHaveBeenCalled()
  })

  it('keeps the explicit GPU price conflict guard', async () => {
    const target = { unit: 'gpu_unit_second', unitQuantity: 1, amount: '0.000100', currency: 'USD', gpuClass: 'nvidia-cuda', gpuMode: 'exclusive' }
    await expect(ensureRateByUi({}, context([rate(target, { gpuClass: target.gpuClass, gpuMode: target.gpuMode })]), target))
      .rejects.toThrow('REAL_WORK_GPU_RATE_ACTIVE_CONFLICT:nvidia-cuda')
  })
})

const VM_ENVIRONMENT = Object.freeze({
  LABWEAVER_E2E_VM_PROVIDER_BINDING: 'kubevirt-primary-v1',
  LABWEAVER_E2E_VM_STORAGE_CLASS_BINDING: 'vm-rwo-primary-v1',
  LABWEAVER_E2E_VM_BASE_DISK_BINDING: 'ubuntu-24.04-vgpu-v1',
  LABWEAVER_E2E_VM_BASE_DISK_SOURCE_REGISTRY_DIGEST: 'docker://quay.io/containerdisks/ubuntu@sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5',
  LABWEAVER_E2E_VM_BASE_DISK_CAPACITY_BYTES: String(10 * GIB),
  LABWEAVER_E2E_VM_SSH_PORT: '22',
})

describe('published Work resume retention', () => {
  const now = Date.parse('2026-10-03T10:15:00.000Z')
  const recovery = 'Generate, review, and publish a new Work template through the normal UI before retrying.'

  afterEach(() => vi.restoreAllMocks())

  function resumedWork(retention) {
    const artifact = { id: 'artifact', kind: 'container', repository: 'harbor.lab.lan/work', digest: `sha256:${'a'.repeat(64)}` }
    const approval = { id: 'approval', candidateId: 'candidate', candidateRevision: 1, policyRevision: 1, trustRevision: 1, decision: 'approved' }
    const release = { id: 'release', projectId: 'project', agentRunId: 'run', candidateId: 'candidate', candidateRevision: 1, version: 1, runtimeKind: 'container', approval, artifact }
    const resume = { projectId: 'project', runId: 'run', releaseId: 'release', seedMarker: 'seed', persistenceMarker: 'persistence' }
    const responses = {
      '/api/v1/auth/session': { actor: { actorId: 'actor' } },
      '/api/v1/projects/project': { id: 'project', ownerActorId: 'actor' },
      '/api/v1/projects/project/agent-runs/run': {
        id: 'run', projectId: 'project', state: 'succeeded', purpose: { kind: 'authoring', environmentClass: 'work' },
        tracks: [{ kind: 'environment', candidateId: 'candidate' }], packageId: 'package',
      },
      '/api/v1/projects/project/environment-candidates/candidate': {
        candidate: { id: 'candidate', runId: 'run', projectId: 'project', revision: 1, policyRevision: 1, spec: { class: 'work', runtime: { kind: 'container' }, retention } },
        trustRevision: 1, approvals: [approval], imageArtifact: artifact, build: { state: 'succeeded', artifact },
      },
      '/api/v1/projects/project/environment-template-releases/release': release,
      '/api/v1/projects/project/problem-packages/package': { id: 'package', projectId: 'project', revision: 1 },
    }
    const request = { get: vi.fn(async (url) => ({ ok: () => true, text: async () => JSON.stringify(responses[url]) })) }
    vi.spyOn(Date, 'now').mockReturnValue(now)
    return { request, resume, release }
  }

  it('continues a bound release with future retention', async () => {
    const { request, resume, release } = resumedWork({ retainUntil: '2026-10-04T10:15:00.123456Z' })
    await expect(readResumablePublishedWork(request, resume)).resolves.toMatchObject({ release })
  })

  it.each(['2026-10-03T10:15:00.000Z', '2026-10-01T14:32:04.364Z'])('rejects retention at or before now: %s', async (retainUntil) => {
    const { request, resume } = resumedWork({ retainUntil })
    await expect(readResumablePublishedWork(request, resume)).rejects.toThrow(`REAL_WORK_RESUME_RETENTION_EXPIRED: ${recovery}`)
    expect(request.get).not.toHaveBeenCalledWith('/api/v1/projects/project/problem-packages/package')
  })

  it.each([undefined, {}, { retainUntil: null }, { retainUntil: 123 }, { retainUntil: '' }, { retainUntil: 'not a timestamp' }])('rejects missing or malformed retention: %j', async (retention) => {
    const { request, resume } = resumedWork(retention)
    await expect(readResumablePublishedWork(request, resume)).rejects.toThrow(`REAL_WORK_RESUME_RETENTION_INVALID: ${recovery}`)
    expect(request.get).not.toHaveBeenCalledWith('/api/v1/projects/project/problem-packages/package')
  })
})
const VM_ENVIRONMENT_KEYS = Object.keys(VM_ENVIRONMENT)
const savedEnvironment = new Map(VM_ENVIRONMENT_KEYS.map((key) => [key, process.env[key]]))
const savedProviderOptIn = process.env.LABWEAVER_E2E_REAL_PROVIDER

function setVmEnvironment(overrides = {}) {
  for (const key of VM_ENVIRONMENT_KEYS) process.env[key] = overrides[key] ?? VM_ENVIRONMENT[key]
}

afterEach(() => {
  for (const [key, value] of savedEnvironment) {
    if (value === undefined) delete process.env[key]
    else process.env[key] = value
  }
  if (savedProviderOptIn === undefined) delete process.env.LABWEAVER_E2E_REAL_PROVIDER
  else process.env.LABWEAVER_E2E_REAL_PROVIDER = savedProviderOptIn
})

describe('real Work VM package', () => {
  it('uses exact reviewed VM catalog values and does not require a container image', () => {
    setVmEnvironment()
    process.env.LABWEAVER_E2E_REAL_PROVIDER = '1'

    expect(realWorkConfig({ virtualMachine: true })).toBeNull()
    expect(realWorkVmConfig()).toEqual({
      providerBinding: 'kubevirt-primary-v1',
      storageClassBinding: 'vm-rwo-primary-v1',
      baseDisk: {
        binding: 'ubuntu-24.04-vgpu-v1',
        sourceRegistryDigest: VM_ENVIRONMENT.LABWEAVER_E2E_VM_BASE_DISK_SOURCE_REGISTRY_DIGEST,
        capacityBytes: 10 * GIB,
      },
      sshPort: 22,
    })
  })

  it('rejects unsafe imported disk capacity values', () => {
    setVmEnvironment({ LABWEAVER_E2E_VM_BASE_DISK_CAPACITY_BYTES: '9007199254740992' })
    expect(() => realWorkVmConfig()).toThrow('LABWEAVER_E2E_VM_BASE_DISK_CAPACITY_INVALID')
  })

  it('builds Work materials with the reviewed VM runtime and mutable authorized guest workspace', async () => {
    setVmEnvironment()
    const vm = realWorkVmConfig()
    const gpu = { class: 'nvidia-v100-2q', count: 1 }
    const packageCopy = await createRealWorkPackage(null, { vm, gpu })
    try {
      const serialized = JSON.parse(await readFile(`${packageCopy.directory}/environment-spec.json`, 'utf8'))
      const readme = await readFile(`${packageCopy.directory}/README.md`, 'utf8')
      const configuration = await readFile(`${packageCopy.directory}/work-configuration.md`, 'utf8')
      const packageFiles = (await readdir(packageCopy.directory)).sort()

      expect(packageCopy.seedMarker).toBeNull()
      expect(packageCopy.dockerfile).toBeNull()
      expect(packageCopy.environmentSpec).toEqual(serialized.environmentSpec)
      expect(packageCopy.environmentSpec).toMatchObject({
        class: 'work',
        resources: {
          cpuMillicores: 1000,
          memoryBytes: 2 * GIB,
          storageBytes: 16 * GIB,
          gpu,
        },
        network: { mode: 'deny_all' },
        entries: [{ name: 'ssh', protocol: 'ssh', servicePort: 22 }],
        security: {
          userPolicy: 'non_root_required',
          rootFilesystemPolicy: 'mutable_required',
          privilegeEscalationPolicy: 'deny',
          publicExposurePolicy: 'deny',
        },
        runtime: {
          kind: 'virtual_machine',
          provider_binding: vm.providerBinding,
          base_disk: vm.baseDisk,
          storage_class_binding: vm.storageClassBinding,
          ssh_port: 22,
        },
      })
      expect(packageCopy.environmentSpec.runtime).not.toHaveProperty('build_recipe')
      expect(readme).toContain(vm.baseDisk.sourceRegistryDigest)
      expect(readme).toContain('Do not replace the VM with a container')
      expect(configuration).toContain('persistence-marker.txt in the current directory')
      expect(configuration).toContain('Do not use sudo or alter the SSH access policy')
      expect(configuration).not.toContain('/workspace/')
      expect(packageFiles).toEqual(['README.md', 'environment-spec.json', 'work-configuration.md'])
    } finally {
      await packageCopy.cleanup()
    }
  })

  it('accepts only the Work candidate and Control-resolved VM artifact matching that catalog entry', async () => {
    setVmEnvironment()
    const vm = realWorkVmConfig()
    const packageCopy = await createRealWorkPackage(null, { vm, gpu: { class: 'nvidia-v100-2q', count: 1 } })
    try {
      const validCandidate = {
        candidate: { spec: packageCopy.environmentSpec },
        build: null,
        imageArtifact: {
          id: '01900000-0000-7000-8000-000000000002',
          kind: 'virtual_machine',
          base_disk: vm.baseDisk,
          format: 'qcow2',
        },
      }
      expect(assertRealWorkVmCandidate(validCandidate, vm, { class: 'nvidia-v100-2q', count: 1 }))
        .toEqual(validCandidate.imageArtifact)
      expect(() => assertRealWorkVmCandidate({
        ...validCandidate,
        imageArtifact: { ...validCandidate.imageArtifact, base_disk: { ...vm.baseDisk, binding: 'other-base' } },
      }, vm, { class: 'nvidia-v100-2q', count: 1 })).toThrow('REAL_WORK_VM_CANDIDATE_ARTIFACT_INVALID')
      expect(() => assertRealWorkVmCandidate({
        ...validCandidate,
        candidate: {
          spec: {
            ...packageCopy.environmentSpec,
            runtime: { ...packageCopy.environmentSpec.runtime, build_recipe: { mode: 'generated' } },
          },
        },
      }, vm, { class: 'nvidia-v100-2q', count: 1 })).toThrow('REAL_WORK_VM_CANDIDATE_SPEC_INVALID')
      expect(() => assertRealWorkVmCandidate({ ...validCandidate, build: { state: 'succeeded' } }, vm))
        .toThrow('REAL_WORK_VM_CANDIDATE_SPEC_INVALID')
    } finally {
      await packageCopy.cleanup()
    }
  })
})


describe('GPU container Work material and candidate', () => {
  const image = `harbor.example.test/python@sha256:${'a'.repeat(64)}`
  const gpu = { class: 'nvidia-cuda', count: 1 }
  const providerBinding = 'container-primary-v1'

  it('preserves the HTTP seed recipe alongside the owner CUDA terminal', async () => {
    const material = await createRealWorkPackage(image, { gpu, providerBinding })
    try {
      expect(material.environmentSpec).toMatchObject({
        class: 'work', resources: { gpu },
        runtime: {
          kind: 'container', provider_binding: providerBinding, service_port: 8080,
          terminal: { executable: '/bin/sh', args: [], workingDirectory: '/workspace' },
        },
      })
      expect(material.dockerfile).toContain(`FROM ${image}`)
      expect(material.dockerfile).toContain('COPY seed.txt /opt/labweaver/workspace-seed/seed.txt')
      expect(material.dockerfile).toContain('"http.server", "8080"')
      const readme = await readFile(`${material.directory}/README.md`, 'utf8')
      expect(readme).toContain('class=work')
      expect(readme).toContain('count 1')
      expect(readme).toContain('256 threads, sum 32640, max 255')
      expect(readme).toContain('libcuda.so.1 must be supplied by the normal NVIDIA runtime')
      expect(() => assertRealWorkGpuContainerCandidate({ candidate: { spec: material.environmentSpec } }, providerBinding, gpu)).not.toThrow()
    } finally {
      await material.cleanup()
    }
  })

  it('rejects changed class, GPU allocation, provider, or terminal before approval', async () => {
    const material = await createRealWorkPackage(image, { gpu, providerBinding })
    try {
      const invalidChanges = [
        (spec) => { spec.class = 'experiment' },
        (spec) => { spec.resources.gpu.class = 'other-gpu' },
        (spec) => { spec.resources.gpu.count = 2 },
        (spec) => { spec.runtime.provider_binding = 'other-provider' },
        (spec) => { delete spec.runtime.terminal },
        (spec) => { spec.runtime.terminal.executable = '/bin/bash' },
        (spec) => { spec.runtime.terminal.args = ['-c', 'true'] },
        (spec) => { spec.runtime.terminal.workingDirectory = '/tmp' },
      ]
      for (const change of invalidChanges) {
        const spec = structuredClone(material.environmentSpec)
        change(spec)
        expect(() => assertRealWorkGpuContainerCandidate({ candidate: { spec } }, providerBinding, gpu))
          .toThrow('REAL_WORK_GPU_CONTAINER_CANDIDATE_SPEC_INVALID')
      }
    } finally {
      await material.cleanup()
    }
  })

  it('keeps CPU container Work without a GPU terminal requirement', async () => {
    const material = await createRealWorkPackage(image, { providerBinding })
    try {
      expect(material.environmentSpec.runtime).not.toHaveProperty('terminal')
      expect(material.environmentSpec.resources).not.toHaveProperty('gpu')
      expect(material.dockerfile).toContain('"http.server", "8080"')
    } finally {
      await material.cleanup()
    }
  })
})

describe('real Work cleanup through public owner APIs', () => {
  const baseURL = 'https://portal.example.test'
  const environmentPath = '/api/v1/environments/environment'
  const leasePath = '/api/v1/resource-leases/lease'
  const requestPath = '/api/v1/resource-requests/request'
  const failed = {
    id: 'environment', projectId: 'project', leaseId: 'lease', capacityBinding: 'claim',
    revision: 4, generation: 2, desiredState: 'deleted', observedState: 'failed',
    operation: { id: 'expire', kind: 'expire', state: 'failed', acceptedRevision: 3 },
  }
  const deleted = { ...failed, revision: 6, generation: 3, observedState: 'deleted', operation: { id: 'delete', kind: 'delete', state: 'succeeded', acceptedRevision: 5 } }
  const expiring = { id: 'lease', requestId: 'request', claimId: 'claim', revision: 3, state: 'expiring' }
  const revoked = { ...expiring, state: 'revoked' }
  const expiredRequest = { id: 'request', projectId: 'project', state: 'expired' }
  const accepted = { environmentId: 'environment', operationId: 'delete', statusUrl: `${environmentPath}/operations/delete` }

  function response(body, status = 200) {
    return { ok: () => status >= 200 && status < 300, status: () => status, text: async () => JSON.stringify(body) }
  }

  function http({ environments = [failed, failed, failed, deleted], leases = [expiring, expiring, revoked], operation = { state: 'succeeded' }, finalRequest = expiredRequest, deletes = [response(accepted)], posts = [response(expiring)] } = {}) {
    const snapshots = new Map([
      [environmentPath, [...environments]], [leasePath, [...leases]],
      [requestPath, [finalRequest]], ['/api/v1/auth/csrf', [{ csrfToken: 'test-csrf' }]],
      [accepted.statusUrl, [operation]],
    ])
    function next(values) {
      if (!values?.length) throw new Error('UNEXPECTED_HTTP_REQUEST')
      return values.length > 1 ? values.shift() : values[0]
    }
    return {
      get: vi.fn(async (path) => response(next(snapshots.get(path)))),
      post: vi.fn(async () => next(posts)),
      delete: vi.fn(async () => {
        const result = next(deletes)
        if (result instanceof Error) throw result
        return result
      }),
    }
  }

  beforeEach(() => {
    // Exercise the real polling predicates once per supplied HTTP snapshot;
    // the live polling duration remains unchanged in the acceptance helper.
    vi.spyOn(playwrightExpect, 'poll').mockImplementation((read) => ({
      toBe: async (value) => expect(await read()).toBe(value),
    }))
  })
  afterEach(() => vi.restoreAllMocks())

  it('releases an active lease through Resource before deleting the environment', async () => {
    const request = http({ environments: [failed, failed, deleted], leases: [{ ...expiring, state: 'active' }, revoked, revoked] })
    await cleanupWorkResources(request, baseURL, 'project', 'environment', 'lease', 'request')
    expect(request.post).toHaveBeenCalledOnce()
    expect(request.post.mock.calls[0][0]).toBe(`${leasePath}/revoke`)
    expect(request.post.mock.invocationCallOrder[0]).toBeLessThan(request.delete.mock.invocationCallOrder[0])
    expect(request.delete).toHaveBeenCalledWith(environmentPath, { headers: expect.objectContaining({ 'If-Match': '"rev-4"', 'X-CSRF-Token': 'test-csrf', Origin: baseURL }) })
  })

  it.each(['terminal failure', 'timeout'])('continues owner DELETE after the first Resource wait ends with %s', async (failure) => {
    const pending = { ...failed, observedState: 'expiring', operation: { ...failed.operation, state: 'running' } }
    const request = http({ environments: [failed, failure === 'timeout' ? pending : failed, failed, deleted] })
    const diagnostic = vi.spyOn(console, 'warn').mockImplementation(() => {})
    await cleanupWorkResources(request, baseURL, 'project', 'environment', 'lease', 'request')
    expect(request.post).not.toHaveBeenCalled()
    expect(request.delete).toHaveBeenCalledOnce()
    expect(diagnostic).toHaveBeenCalledWith('REAL_WORK_CLEANUP_RECOVERED:RESOURCE_RELEASE_WAIT_FAILED')
    expect(request.get).toHaveBeenCalledWith(requestPath)
  })

  it('aggregates failed delete and unconfirmed release without hiding either', async () => {
    const request = http({ operation: { state: 'failed' }, leases: [expiring] })
    let failure
    try { await cleanupWorkResources(request, baseURL, 'project', 'environment', 'lease', 'request') } catch (error) { failure = error }
    expect(failure).toBeInstanceOf(AggregateError)
    expect(failure.errors).toHaveLength(3)
    expect(failure.message).toContain('REAL_WORK_CLEANUP_ENVIRONMENT_DELETE_OPERATION_FAILED:failed')
    expect(request.delete).toHaveBeenCalledOnce()
  })

  it('requires the Resource request to settle even after deleted and revoked readbacks', async () => {
    const request = http({ finalRequest: { ...expiredRequest, state: 'expiring' } })
    await expect(cleanupWorkResources(request, baseURL, 'project', 'environment', 'lease', 'request')).rejects.toBeInstanceOf(AggregateError)
    expect(request.get).toHaveBeenCalledWith(requestPath)
  })

  it.each([
    { id: 'another-request', projectId: 'project', state: 'expired' },
    { id: 'request', projectId: 'another-project', state: 'expired' },
    { requestId: 'request', projectId: 'project', state: 'expired' },
  ])('rejects terminal Resource readback with a different public identity: %j', async (finalRequest) => {
    const request = http({ finalRequest })
    await expect(cleanupWorkResources(request, baseURL, 'project', 'environment', 'lease', 'request')).rejects.toBeInstanceOf(AggregateError)
    expect(request.get).toHaveBeenCalledWith(requestPath)
  })

  it('does not treat a forbidden delete as absence or retry it', async () => {
    const request = http({ deletes: [response({ diagnosticCode: 'LW_SCOPE_DENIED' }, 403)] })
    await expect(cleanupWorkResources(request, baseURL, 'project', 'environment', 'lease', 'request')).rejects.toThrow('REAL_WORK_CLEANUP_ENVIRONMENT_DELETE_FAILED:403')
    expect(request.delete).toHaveBeenCalledOnce()
  })

  it('retains a forbidden Resource wait read even if later owner cleanup succeeds', async () => {
    const request = http({ environments: [failed, failed, deleted] })
    const get = request.get.getMockImplementation()
    let environmentReads = 0
    request.get.mockImplementation(async (path) => {
      if (path === environmentPath && ++environmentReads === 2) return response({ diagnosticCode: 'LW_SCOPE_DENIED' }, 403)
      return get(path)
    })
    await expect(cleanupWorkResources(request, baseURL, 'project', 'environment', 'lease', 'request')).rejects.toThrow('REAL_WORK_CLEANUP_ENVIRONMENT_RELEASE_READ_FAILED:403')
    expect(request.delete).toHaveBeenCalledOnce()
  })

  it('recovers a lost delete response by reading the exact next delete generation', async () => {
    const committed = { ...deleted, observedState: 'deleting', operation: { ...deleted.operation, state: 'running' } }
    const request = http({ environments: [failed, failed, failed, committed, deleted], deletes: [new Error('transport lost')] })
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    await cleanupWorkResources(request, baseURL, 'project', 'environment', 'lease', 'request')
    expect(request.delete).toHaveBeenCalledOnce()
    expect(request.get).toHaveBeenCalledWith(accepted.statusUrl)
  })

  it('retains one logical key and revision when no delete was committed', async () => {
    const request = http({ environments: [failed, failed, failed, failed, deleted], deletes: [new Error('transport lost'), response(accepted)] })
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    await cleanupWorkResources(request, baseURL, 'project', 'environment', 'lease', 'request')
    expect(request.delete).toHaveBeenCalledTimes(2)
    expect(request.delete.mock.calls[1]).toEqual(request.delete.mock.calls[0])
  })

  it('does not start another intent when a lost response has a different generation', async () => {
    const unrelated = { ...deleted, generation: 4, operation: { ...deleted.operation, id: 'unrelated' } }
    const request = http({ environments: [failed, failed, failed, unrelated], deletes: [new Error('transport lost')] })
    await expect(cleanupWorkResources(request, baseURL, 'project', 'environment', 'lease', 'request')).rejects.toThrow('REAL_WORK_CLEANUP_DELETE_RESPONSE_LOST_UNCONFIRMED')
    expect(request.delete).toHaveBeenCalledOnce()
  })
})
