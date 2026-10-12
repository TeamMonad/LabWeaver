import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import RuntimePermissions from '@/components/review/RuntimePermissions.vue'
import {
  runtimeAuthorizationReceiptGroups,
  runtimePermissionGroups,
} from '@/utils/runtimePermissions'
import type { EnvironmentRuntimeAuthorizationResolution } from '@/generated/contracts'
import { environmentCandidateView, workCandidateView } from '../fixtures/authoring'

const sampleReceipt = {
  authorization: {
    basis: 'project_preauthorization',
    configuration: {
      authoringProjectId: 'project-1',
      candidateId: 'candidate-env-work',
      candidateRevision: 1,
      configurationSha256: 'sha256:config',
    },
    permissions: {
      capabilities: ['NET_BIND_SERVICE'],
      privileged: false,
      scope: { kind: 'project', projectId: 'project-1' },
    },
    platformConfigurationRevision: 7,
    platformConfigurationSha256: 'sha256:platform',
    projectId: 'project-1',
    requestGeneration: 1,
    requestId: 'request-1',
    target: {
      kind: 'environment',
      environmentId: 'env-1',
      releaseId: 'release-1',
      releaseVersion: 3,
    },
  },
  defaultContainerUid: 65534,
  leaseAuthorization: {
    activeFrom: '2026-10-12T00:00:00.000Z',
    approvedResources: { cpuMillicores: 1000, memoryBytes: 1073741824, storageBytes: 10737418240 },
    capacityBinding: 'capacity-v1',
    environmentId: 'env-1',
    expiresAt: '2026-10-13T00:00:00.000Z',
    leaseId: 'lease-1',
    leaseRevision: 1,
    ownerActorId: 'actor-1',
    projectId: 'project-1',
    resourceRequestId: 'request-1',
  },
  ownerActorId: 'actor-1',
  providerBinding: 'container-primary-v1',
  validUntil: '2026-10-13T00:00:00.000Z',
} as unknown as EnvironmentRuntimeAuthorizationResolution

describe('RuntimePermissions (#207 运行权限展示)', () => {
  it('reads runAsUser / privileged / capabilities / seccomp from the #258 security surface', () => {
    const groups = runtimePermissionGroups(environmentCandidateView.candidate.spec)
    const rows = Object.fromEntries(groups.flatMap((group) => group.rows.map((row) => [row.label, row])))
    expect(rows['运行用户'].value).toContain('UID 65534')
    expect(rows['系统目录']).toMatchObject({
      value: '只读根文件系统（可写工作目录与独立 /tmp）',
      tone: 'restricted',
    })
    expect(rows['系统目录'].hint).toContain('独立 /tmp')
    expect(rows['提升权限']).toMatchObject({ value: '禁止（容器 no_new_privs）', tone: 'restricted' })
    expect(rows['提升权限'].hint).toContain('setuid')
    expect(rows['特权模式']).toMatchObject({ value: '关闭（默认）', tone: 'restricted' })
    expect(rows['Capabilities'].value).toContain('无额外能力')
    expect(rows['seccomp'].value).toBe('runtime_default')
    expect(rows['网络'].value).toContain('只能访问平台允许的地址')
    expect(groups.some((group) => group.label === '设备')).toBe(false)
  })

  it('marks a writable container system directory as not kept across rebuilds, and shows the GPU', () => {
    const groups = runtimePermissionGroups(workCandidateView.candidate.spec)
    const rows = groups.flatMap((group) => group.rows)
    const system = rows.find((row) => row.label === '系统目录')!
    expect(system).toMatchObject({ value: '可写', tone: 'open' })
    expect(system.hint).toContain('容器重建后系统目录的修改不会保留')
    expect(rows.find((row) => row.label === 'GPU')?.value).toBe('nvidia-l4 × 1')
    expect(rows.filter((row) => row.label === '入口').map((row) => row.value)).toEqual(['jupyter · HTTPS · 端口 8888', 'ssh · SSH · 端口 22'])
  })

  it('shows the runtimeAuthorization receipt when provided, and only residual admin-scope gaps', () => {
    const wrapper = mount(RuntimePermissions, {
      props: { spec: workCandidateView.candidate.spec, runtimeAuthorization: sampleReceipt },
    })
    const receipt = wrapper.get('[data-testid="runtime-authorization-receipt"]').text()
    expect(receipt).toContain('request-1')
    expect(receipt).toContain('NET_BIND_SERVICE')
    expect(receipt).toContain('lease-1')
    const gap = wrapper.get('[data-testid="permission-gap"]').text()
    expect(gap).toContain('管理员策略允许的能力上限')
    expect(gap).not.toContain('特权模式（privileged）是否开启')
    expect(wrapper.text()).not.toMatch(/已获准|未获准/)
  })

  it('builds receipt groups from the original ResourceApproval projection', () => {
    const groups = runtimeAuthorizationReceiptGroups(sampleReceipt)
    const rows = Object.fromEntries(groups[0].rows.map((row) => [row.label, row.value]))
    expect(rows['资源请求']).toBe('request-1')
    expect(rows['已批准能力']).toContain('NET_BIND_SERVICE')
    expect(rows['默认容器 UID']).toBe('65534')
  })

  it('states that privileged execution is not open in this version', () => {
    const spec = structuredClone(environmentCandidateView.candidate.spec)
    spec.security.privileged = true
    spec.security.capabilities = []
    const rows = Object.fromEntries(
      runtimePermissionGroups(spec).flatMap((group) => group.rows.map((row) => [row.label, row])),
    )
    expect(rows['特权模式'].hint).toContain('本版本尚未开放实际特权执行')
    expect(rows['特权模式'].hint).not.toContain('除非已验证执行位置')
    expect(rows['Capabilities'].value).toContain('特权模式声明包含全部能力')
    expect(rows['Capabilities'].value).not.toContain('drop ALL')
  })

  it('keeps root UID independent from privileged, and clarifies VM escalation semantics', () => {
    const containerRoot = structuredClone(environmentCandidateView.candidate.spec)
    containerRoot.security.runAsUser = 0
    containerRoot.security.privileged = false
    const containerRows = Object.fromEntries(
      runtimePermissionGroups(containerRoot).flatMap((group) => group.rows.map((row) => [row.label, row])),
    )
    expect(containerRows['运行用户'].hint).toContain('不等于 privileged')
    expect(containerRows['特权模式'].value).toContain('关闭')

    const vmSpec = structuredClone(environmentCandidateView.candidate.spec)
    vmSpec.runtime = {
      kind: 'virtual_machine',
      provider_binding: 'qemu-default',
      ssh_port: 22,
      storage_class_binding: 'vm-disk-v1',
      base_disk: {
        artifactId: 'img-1',
        mediaType: 'application/x-qcow2',
        objectVersion: 'v1',
        sha256: 'sha256:bb',
        sizeBytes: 1,
      },
    } as typeof vmSpec.runtime
    vmSpec.security.privilegeEscalationPolicy = 'deny'
    const vmRows = Object.fromEntries(
      runtimePermissionGroups(vmSpec).flatMap((group) => group.rows.map((row) => [row.label, row])),
    )
    expect(vmRows['提升权限'].hint).toContain('guest OS')
    expect(vmRows['提升权限'].value).toContain('容器字段语义')
  })

})
