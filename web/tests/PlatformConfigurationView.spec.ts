import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type DOMWrapper, type VueWrapper } from '@vue/test-utils'
import PlatformConfigurationView from '@/views/admin/PlatformConfigurationView.vue'
import {
  applyPlatformConfiguration,
  getPlatformConfiguration,
  registerPlatformConfigurationBundle,
  updatePlatformConfiguration,
} from '@/generated/contracts'
import type { PlatformConfigurationDesired, PlatformConfigurationStatusSchema } from '@/generated/contracts'

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

const configuration: PlatformConfigurationDesired = {
  lifecycle: { readOnlyRetentionSeconds: 604800, detailedLogRetentionSeconds: 2592000, expiryReminderSeconds: [86400, 3600], preemptionGraceSeconds: 60 },
  runtimePolicy: { defaultContainerUid: 0, allowedCapabilities: [], preauthorizations: [], privilegedAllowed: false, privilegedExecutionLocation: null },
  builderPolicy: { capabilities: [], allowPrivilegeEscalation: false, entitlements: [] },
}

const configurationSha = 'a'.repeat(64)
const bundleSha = 'd'.repeat(64)

function statusWithoutRegistration(desired: PlatformConfigurationDesired = configuration): PlatformConfigurationStatusSchema {
  return {
    revision: 4,
    appliedState: 'unverified',
    defaults: configuration,
    desired: { revision: 4, configuration: desired, configurationSha256: configurationSha, createdAt: '2026-09-14T08:00:00Z', createdBy: 'admin-a' },
    applied: null,
    registeredBundle: null,
    latestApply: {
      id: 'apply-1', desiredRevision: 3, encryptedBundleSha256: 'b'.repeat(64), stage: 'failed', failedStage: 'applying',
      failureReason: 'external_result_unknown', nextStep: 'reconcile_external_result', requestedAt: '2026-09-14T07:00:00Z',
      requestedBy: 'admin-a', revision: 3, updatedAt: '2026-09-14T07:30:00Z',
    },
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

function problem(status: number, code: string) {
  return {
    data: undefined,
    error: { type: 'urn:x', title: 'Control request blocked', status, detail: 'The request was rejected by a fail-closed control-plane boundary.', instance: 'urn:labweaver:request:req-9', requestId: 'req-9', diagnosticCode: code, retryable: false },
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

/**
 * The banner leads with a localized reason and a next step; the code and requestId follow in the
 * collapsed details. The generic English Control `detail` is never the visible reason.
 */
function expectDiagnosticOrder(banner: DOMWrapper<Element>, code: string, requestId: string): DOMWrapper<Element> {
  expect(banner.exists()).toBe(true)
  const message = banner.find('.diagnostic-message')
  const nextStep = banner.find('.diagnostic-next-step')
  const codeNode = banner.find('.diagnostic-code')
  const requestNode = banner.find('.diagnostic-request-id')
  expect(message.text()).toMatch(/[\u4e00-\u9fff]/)
  expect(banner.text()).not.toContain('fail-closed')
  expect(nextStep.text()).toMatch(/[\u4e00-\u9fff]/)
  expect(codeNode.text()).toBe(code)
  expect(requestNode.text()).toBe(requestId)
  const follows = (a: Element, b: Element) => Boolean(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING)
  expect(follows(message.element, nextStep.element)).toBe(true)
  expect(follows(nextStep.element, codeNode.element)).toBe(true)
  expect(follows(codeNode.element, requestNode.element)).toBe(true)
  return banner
}

async function chooseFile(wrapper: VueWrapper, file: File) {
  const input = wrapper.find('[data-testid="pc-bundle-file"]')
  Object.defineProperty(input.element, 'files', { value: [file] })
  await input.trigger('change')
  await flushPromises()
}

describe('PlatformConfigurationView', () => {
  it('shows desired, the verified state, impact, attempt history and missing registration from server facts', async () => {
    const wrapper = await mountView()
    expect(wrapper.find('[data-testid="pc-applied-state"]').text()).toBe('待核对')
    expect(wrapper.find('[data-testid="pc-impact"]').text()).toContain('运行策略')
    expect(wrapper.text()).toContain('revision 4')
    expect(wrapper.find('[data-testid="pc-registered-bundle"]').text()).toContain('无')
    const history = wrapper.find('[data-testid="pc-apply-history"]')
    expect(history.find('tbody tr.data-table__row').text()).toContain('外部结果未知（应用中）')
    expect(history.text()).toContain('先核对外部真实状态')
    expect(wrapper.find('[data-testid="pc-apply"]').attributes('disabled')).toBeDefined()
    expect(wrapper.find('[data-testid="pc-apply-needs-register"]').exists()).toBe(true)
  })

  it('offers only deployable runtime and builder settings and labels the rest as having no execution-side implementation', async () => {
    const wrapper = await mountView()
    const form = wrapper.find('.pc-form')
    // No editable capability, preauthorization, UID or builder controls exist.
    expect(form.findAll('input[type="checkbox"]')).toHaveLength(0)
    expect(form.text()).not.toContain('添加预授权')
    expect(form.find('[data-testid="pc-runtime-fixed"]').text()).toContain('默认容器 UID')
    expect(form.text()).toContain('尚无执行侧实现')
    const privileged = form.find('select[data-testid="pc-privileged-mode"]')
    const vmOption = privileged.find('option[value="virtual_machine_guest"]')
    expect(vmOption.attributes('disabled')).toBeDefined()
    expect(vmOption.text()).toContain('尚无执行侧实现')

    await privileged.setValue('dedicated_node_pool')
    await flushPromises()
    expect(wrapper.find('[data-testid="pc-node-pool"]').exists()).toBe(true)
  })

  it('registers a selected .age file then confirms apply through ConfirmDialog without a hand-filled digest', async () => {
    const registered = statusWithRegistration()
    vi.mocked(registerPlatformConfigurationBundle).mockResolvedValue({ data: registered, error: undefined } as never)
    vi.mocked(applyPlatformConfiguration).mockResolvedValue({ data: registered, error: undefined } as never)
    const wrapper = await mountView()

    const file = new File([new Uint8Array(128)], 'platform.age', { type: 'application/octet-stream' })
    await chooseFile(wrapper, file)
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
    expect(wrapper.find('[data-testid="pc-apply-registered"]').text()).toContain(bundleSha)
    expect(wrapper.find('[data-testid="pc-apply"]').attributes('disabled')).toBeUndefined()

    await wrapper.find('[data-testid="pc-apply"]').trigger('click')
    await flushPromises()
    const dialog = document.body.querySelector('[role="alertdialog"]')
    expect(dialog?.textContent).toContain('浏览器不会执行部署')
    expect(dialog?.textContent).toContain(bundleSha.slice(0, 12))
    expect(dialog?.querySelector('input')).toBeNull()
    const confirm = [...document.body.querySelectorAll('[role="alertdialog"] button')].find((button) => button.textContent?.includes('记录应用请求')) as HTMLButtonElement
    confirm.click()
    await flushPromises()

    expect(vi.mocked(applyPlatformConfiguration)).toHaveBeenCalledTimes(1)
    const applyBody = (vi.mocked(applyPlatformConfiguration).mock.calls[0][0] as { body: Record<string, unknown> }).body
    expect(applyBody).toEqual({ expectedRevision: 5, desiredRevision: 4 })
    expect(applyBody).not.toHaveProperty('encryptedBundleSha256')
    expect(document.body.querySelector('[role="alertdialog"]')).toBeNull()
    expect(wrapper.find('[data-testid="pc-notice"]').text()).toContain('202')
  })

  it('opens the comparison on a 412 and keeps the draft and selected file', async () => {
    vi.mocked(getPlatformConfiguration)
      .mockResolvedValueOnce({ data: statusWithoutRegistration(), error: undefined } as never)
      .mockResolvedValue({ data: { ...statusWithoutRegistration(), revision: 9 }, error: undefined } as never)
    vi.mocked(registerPlatformConfigurationBundle).mockResolvedValue(problem(412, 'LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT') as never)
    const wrapper = await mountView()
    await chooseFile(wrapper, new File([new Uint8Array(64)], 'platform.age'))

    await wrapper.find('[data-testid="pc-register"]').trigger('click')
    await flushPromises()

    expect(wrapper.find('[data-testid="pc-conflict"]').text()).toContain('revision 9')
    expectDiagnosticOrder(wrapper.find('[data-testid="pc-register-failure"]'), 'LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT', 'req-9')
    expect(wrapper.find('[data-testid="pc-bundle-file-name"]').text()).toContain('platform.age')
  })

  it('shows the Chinese reason first when the server rejects the policy, never the generic English detail', async () => {
    vi.mocked(updatePlatformConfiguration).mockResolvedValue(problem(422, 'LW_PLATFORM_CONFIGURATION_LIFECYCLE_INVALID') as never)
    const wrapper = await mountView()
    await wrapper.find('[data-testid="pc-preemption-grace"]').setValue('301')
    await wrapper.find('[data-testid="pc-save"]').trigger('click')
    await flushPromises()

    const banner = expectDiagnosticOrder(wrapper.find('[data-testid="pc-save-failure"]'), 'LW_PLATFORM_CONFIGURATION_LIFECYCLE_INVALID', 'req-9')
    expect(banner.find('.diagnostic-next-step').text()).toContain('1–300')
  })

  it('marks a saved desired with unsupported settings and blocks registration and apply', async () => {
    const unsupported = structuredClone(configuration)
    unsupported.runtimePolicy.allowedCapabilities = ['SYS_PTRACE']
    unsupported.builderPolicy.entitlements = ['network.host']
    vi.mocked(getPlatformConfiguration).mockResolvedValue({ data: statusWithoutRegistration(unsupported), error: undefined } as never)
    const wrapper = await mountView()

    const banner = wrapper.find('[data-testid="pc-desired-unsupported"]')
    expect(banner.text()).toContain('普通容器 capabilities 允许清单')
    expect(banner.text()).toContain('builder entitlements')
    expect(wrapper.find('[data-testid="pc-bundle-file"]').attributes('disabled')).toBeDefined()
    expect(wrapper.find('[data-testid="pc-register"]').attributes('disabled')).toBeDefined()
    expect(wrapper.find('[data-testid="pc-apply"]').attributes('disabled')).toBeDefined()
    expect(wrapper.find('[data-testid="pc-save"]').attributes('disabled')).toBeDefined()

    await wrapper.find('[data-testid="pc-reset-unsupported"]').trigger('click')
    await flushPromises()
    expect(wrapper.find('[data-testid="pc-draft-unsupported"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="pc-save"]').attributes('disabled')).toBeUndefined()
  })

  it('shows the administrator-only reason when a non-administrator opens the page', async () => {
    vi.mocked(getPlatformConfiguration).mockResolvedValue(problem(403, 'LW_FORBIDDEN') as never)
    const wrapper = await mountView()
    expectDiagnosticOrder(wrapper.find('.diagnostic-banner'), 'LW_FORBIDDEN', 'req-9')
    expect(wrapper.find('.pc-form').exists()).toBe(false)
    expect(wrapper.find('[data-testid="pc-save"]').exists()).toBe(false)
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
