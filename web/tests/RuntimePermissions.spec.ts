import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import RuntimePermissions from '@/components/review/RuntimePermissions.vue'
import { runtimePermissionGroups } from '@/utils/runtimePermissions'
import { environmentCandidateView, workCandidateView } from '../fixtures/authoring'

describe('RuntimePermissions (#207 运行权限展示)', () => {
  it('reads a restricted experiment environment', () => {
    const groups = runtimePermissionGroups(environmentCandidateView.candidate.spec)
    const rows = Object.fromEntries(groups.flatMap((group) => group.rows.map((row) => [row.label, row])))
    expect(rows['运行用户'].value).toBe('普通用户（不能使用 root）')
    expect(rows['系统目录']).toMatchObject({ value: '只读，只能写工作目录', tone: 'restricted' })
    expect(rows['提升权限'].tone).toBe('restricted')
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

  it('lists the #207 fields the contract does not carry instead of guessing them', () => {
    const wrapper = mount(RuntimePermissions, { props: { spec: workCandidateView.candidate.spec } })
    const gap = wrapper.get('[data-testid="permission-gap"]').text()
    expect(gap).toContain('特权模式（privileged）是否开启')
    expect(gap).toContain('capabilities')
    expect(gap).toContain('seccomp')
    expect(gap).toContain('管理员策略允许的范围')
    expect(wrapper.text()).not.toMatch(/privileged=(true|false)|已获准|未获准/)
  })
})
