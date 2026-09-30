import { readFile, readdir } from 'node:fs/promises'
import { afterEach, describe, expect, it } from 'vitest'
import {
  assertRealWorkVmCandidate,
  createRealWorkPackage,
  realWorkConfig,
  realWorkVmConfig,
} from '../e2e/support/real-work.mjs'

const GIB = 1024 ** 3
const VM_ENVIRONMENT = Object.freeze({
  LABWEAVER_E2E_VM_PROVIDER_BINDING: 'kubevirt-primary-v1',
  LABWEAVER_E2E_VM_STORAGE_CLASS_BINDING: 'vm-rwo-primary-v1',
  LABWEAVER_E2E_VM_BASE_DISK_BINDING: 'ubuntu-24.04-vgpu-v1',
  LABWEAVER_E2E_VM_BASE_DISK_SOURCE_REGISTRY_DIGEST: 'docker://quay.io/containerdisks/ubuntu@sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5',
  LABWEAVER_E2E_VM_BASE_DISK_CAPACITY_BYTES: String(10 * GIB),
  LABWEAVER_E2E_VM_SSH_PORT: '22',
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
