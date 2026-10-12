import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import PlatformConfigurationView from '@/views/admin/PlatformConfigurationView.vue'
import {
  applyPlatformConfiguration,
  getPlatformConfiguration,
  registerPlatformConfigurationBundle,
} from '@/generated/contracts'
import type { PlatformConfigurationStatusSchema } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getPlatformConfiguration: vi.fn(),
    applyPlatformConfiguration: vi.fn(),
    registerPlatformConfigurationBundle: vi.fn(),
    updatePlatformConfiguration: vi.fn(),
  }
})

const configuration = {
  lifecycle: { readOnlyRetentionSeconds: 604800, detailedLogRetentionSeconds: 2592000, expiryReminderSeconds: [86400, 3600], preemptionGraceSeconds: 60 },
  runtimePolicy: { defaultContainerUid: 0, allowedCapabilities: [], preauthorizations: [], privilegedAllowed: false, privilegedExecutionLocation: null },
  builderPolicy: { capabilities: [], allowPrivilegeEscalation: false, entitlements: [] },
}

const configurationSha = 'a'.repeat(64)
const bundleSha = 'd'.repeat(64)

function statusWithoutRegistration(): PlatformConfigurationStatusSchema {
  return {
    revision: 4,
    appliedState: 'unverified',
    defaults: configuration,
    desired: { revision: 4, configuration, configurationSha256: configurationSha, createdAt: '2026-09-14T08:00:00Z', createdBy: 'admin-a' },
    applied: null,
    registeredBundle: null,
    latestApply: null,
    impact: { runtimePolicyChanged: true, builderPolicyChanged: false, lifecyclePolicyChanged: false },
  }
}

function statusWithRegistration(): PlatformConfigurationStatusSchema {
  return {
    ...statusWithoutRegistration(),
    revision: 5,
    registeredBundle: {
      desiredRevision: 4,
      configurationSha256: configurationSha,
      encryptedBundleSha256: bundleSha,
      encryptedBytes: 4096,
      registeredAtRevision: 5,
      registeredBy: 'admin-a',
      registeredAt: '2026-09-14T08:10:00Z',
    },
  }
}

const mounted: VueWrapper[] = []

beforeEach(() => {
  vi.mocked(getPlatformConfiguration).mockResolvedValue({ data: statusWithoutRegistration(), error: undefined } as never)
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
  it('shows desired, the verified state, impact and missing registration from server facts', async () => {
    const wrapper = await mountView()
    expect(wrapper.find('[data-testid="pc-applied-state"]').text()).toBe('待核对')
    expect(wrapper.find('[data-testid="pc-impact"]').text()).toContain('运行策略')
    expect(wrapper.text()).toContain('revision 4')
    expect(wrapper.find('[data-testid="pc-registered-bundle"]').text()).toContain('无')
    expect(wrapper.find('[data-testid="pc-apply"]').attributes('disabled')).toBeDefined()
    expect(wrapper.find('[data-testid="pc-apply-needs-register"]').exists()).toBe(true)
  })

  it('registers a selected .age file then confirms apply without a hand-filled digest', async () => {
    const registered = statusWithRegistration()
    vi.mocked(registerPlatformConfigurationBundle).mockResolvedValue({ data: registered, error: undefined } as never)
    vi.mocked(applyPlatformConfiguration).mockResolvedValue({ data: registered, error: undefined } as never)
    const wrapper = await mountView()

    const input = wrapper.find('[data-testid="pc-bundle-file"]').element as HTMLInputElement
    const file = new File([new Uint8Array(128)], 'platform.age', { type: 'application/octet-stream' })
    Object.defineProperty(input, 'files', { value: [file] })
    await wrapper.find('[data-testid="pc-bundle-file"]').trigger('change')
    await flushPromises()
    expect(wrapper.find('[data-testid="pc-bundle-file-name"]').text()).toContain('platform.age')

    await wrapper.find('[data-testid="pc-register"]').trigger('click')
    await flushPromises()
    expect(vi.mocked(registerPlatformConfigurationBundle)).toHaveBeenCalledTimes(1)
    const registerArgs = vi.mocked(registerPlatformConfigurationBundle).mock.calls[0][0] as {
      path: { revision: number }
      query: { expectedRevision: number; configurationSha256: string }
      body: File
    }
    expect(registerArgs.path).toEqual({ revision: 4 })
    expect(registerArgs.query).toEqual({ expectedRevision: 4, configurationSha256: configurationSha })
    expect(registerArgs.body).toBe(file)
    expect(wrapper.find('[data-testid="pc-registered-bundle"]').text()).toContain(bundleSha.slice(0, 12))
    expect(wrapper.find('[data-testid="pc-apply"]').attributes('disabled')).toBeUndefined()

    await wrapper.find('[data-testid="pc-apply"]').trigger('click')
    await flushPromises()
    const dialog = document.body.querySelector('[role="alertdialog"]')
    expect(dialog?.textContent).toContain('不会在浏览器中执行部署')
    expect(dialog?.textContent).toContain('待核对')
    expect(dialog?.querySelector('[data-testid="pc-bundle-sha"]')).toBeNull()
    expect(dialog?.querySelector('[data-testid="pc-apply-registered"]')?.textContent).toContain(bundleSha)
    const confirm = document.body.querySelector('[data-testid="pc-apply-confirm"]') as HTMLButtonElement
    expect(confirm.disabled).toBe(false)
    confirm.click()
    await flushPromises()

    expect(vi.mocked(applyPlatformConfiguration)).toHaveBeenCalledTimes(1)
    const applyBody = (vi.mocked(applyPlatformConfiguration).mock.calls[0][0] as { body: Record<string, unknown> }).body
    expect(applyBody).toEqual({ expectedRevision: 5, desiredRevision: 4 })
    expect(applyBody).not.toHaveProperty('encryptedBundleSha256')
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
