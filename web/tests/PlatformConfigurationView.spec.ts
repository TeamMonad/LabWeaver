import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import PlatformConfigurationView from '@/views/admin/PlatformConfigurationView.vue'
import { applyPlatformConfiguration, getPlatformConfiguration } from '@/generated/contracts'
import type { PlatformConfigurationStatusSchema } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, getPlatformConfiguration: vi.fn(), applyPlatformConfiguration: vi.fn(), updatePlatformConfiguration: vi.fn() }
})

const configuration = {
  lifecycle: { readOnlyRetentionSeconds: 604800, detailedLogRetentionSeconds: 2592000, expiryReminderSeconds: [86400, 3600], preemptionGraceSeconds: 60 },
  runtimePolicy: { defaultContainerUid: 0, allowedCapabilities: [], preauthorizations: [], privilegedAllowed: false, privilegedExecutionLocation: null },
  builderPolicy: { capabilities: [], allowPrivilegeEscalation: false, entitlements: [] },
}

const status: PlatformConfigurationStatusSchema = {
  revision: 4,
  appliedState: 'unverified',
  defaults: configuration,
  desired: { revision: 4, configuration, configurationSha256: 'a'.repeat(64), createdAt: '2026-09-14T08:00:00Z', createdBy: 'admin-a' },
  applied: null,
  latestApply: null,
  impact: { runtimePolicyChanged: true, builderPolicyChanged: false, lifecyclePolicyChanged: false },
}

const mounted: VueWrapper[] = []

beforeEach(() => {
  vi.mocked(getPlatformConfiguration).mockResolvedValue({ data: status, error: undefined } as never)
})

afterEach(() => {
  while (mounted.length) mounted.pop()?.unmount()
  vi.resetAllMocks()
})

async function mountView() {
  const wrapper = mount(PlatformConfigurationView, { attachTo: document.body })
  mounted.push(wrapper)
  await flushPromises()
  return wrapper
}

describe('PlatformConfigurationView', () => {
  it('shows desired, the verified state and impact from server facts', async () => {
    const wrapper = await mountView()
    expect(wrapper.find('[data-testid="pc-applied-state"]').text()).toBe('待核对')
    expect(wrapper.find('[data-testid="pc-impact"]').text()).toContain('运行策略')
    expect(wrapper.text()).toContain('revision 4')
  })

  it('confirms an apply request in an alert dialog and requires a bundle digest', async () => {
    vi.mocked(applyPlatformConfiguration).mockResolvedValue({ data: status, error: undefined } as never)
    const wrapper = await mountView()
    await wrapper.find('[data-testid="pc-apply"]').trigger('click')
    await flushPromises()

    const dialog = document.body.querySelector('[role="alertdialog"]')
    expect(dialog?.textContent).toContain('不会在浏览器中执行部署')
    expect(dialog?.textContent).toContain('待核对')
    const confirm = document.body.querySelector('[data-testid="pc-apply-confirm"]') as HTMLButtonElement
    expect(confirm.disabled).toBe(true)

    const input = document.body.querySelector('[data-testid="pc-bundle-sha"]') as HTMLInputElement
    input.value = 'd'.repeat(64)
    input.dispatchEvent(new Event('input'))
    await flushPromises()
    expect(confirm.disabled).toBe(false)
    confirm.click()
    await flushPromises()

    expect(vi.mocked(applyPlatformConfiguration)).toHaveBeenCalledTimes(1)
    expect(document.body.querySelector('[role="alertdialog"]')).toBeNull()
    expect(wrapper.find('[data-testid="pc-notice"]').text()).toContain('202')
  })

  it('switches between the form and the advanced YAML of the same request', async () => {
    const wrapper = await mountView()
    const yamlTab = wrapper.findAll('[role="tab"]').find((tab) => tab.text() === '高级 YAML')!
    await yamlTab.trigger('mousedown', { button: 0 })
    await flushPromises()
    const yaml = wrapper.find('[data-testid="pc-yaml"]').element as HTMLTextAreaElement
    expect(yaml.value).toContain('expectedRevision: 4')
    expect(yaml.value).toContain('preemptionGraceSeconds: 60')
  })
})
