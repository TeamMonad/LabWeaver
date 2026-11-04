import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'
import ProjectCreditsPanel from '@/components/console/ProjectCreditsPanel.vue'
import {
  createCreditAdminOperation,
  getProjectBuildCost,
  getProjectCredits,
  listProjectCreditHolds,
  listProjectCreditLedger,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    createCreditAdminOperation: vi.fn(),
    getProjectBuildCost: vi.fn(),
    getProjectCredits: vi.fn(),
    listProjectCreditHolds: vi.fn(),
    listProjectCreditLedger: vi.fn(),
  }
})

const authState = vi.hoisted(() => ({ user: { value: null as { profile?: unknown } | null } }))
vi.mock('@/composables/useAuth', () => ({ useAuth: () => authState }))

const projectsState = reactive({
  projects: { kind: 'success' as const, data: [
    { id: 'project-a', name: '项目 A', courseId: null },
    { id: 'project-b', name: '项目 B', courseId: null },
  ] },
  selectedProjectId: 'project-a',
})
vi.mock('@/composables/useProjects', () => ({ useProjects: () => projectsState }))

const account = {
  projectId: 'project-a', currency: 'CREDITS', balance: '20.000000', available: '20.000000', reserved: '0.000000',
  spent: '0.000000', pending: '0.000000', unknown: '0.000000', unknownCount: 0, platformAbsorbed: '0.000000', revision: 1,
}
const holdEntry = {
  id: 'entry-hold', kind: 'hold', operationId: 'op-h', reason: 'metered usage', balanceDelta: '0.000000', reservedDelta: '5.000000',
  spentDelta: '0.000000', platformAbsorbed: '0.000000', createdAt: '2026-10-12T09:00:00.000Z', rateSnapshots: [], holdId: 'hold-1',
}
const grantEntry = { ...holdEntry, id: 'entry-grant', kind: 'grant', reason: '课程额度', balanceDelta: '20.000000', reservedDelta: '0.000000', holdId: null }
const modelHold = {
  id: 'hold-model', projectId: 'project-a', amount: '2.000000', remaining: '2.000000', state: 'unknown', measurementUnknown: true,
  businessKey: 'model-call', ownerClientId: 'agent-service', createdAt: '2026-10-12T09:00:00.000Z', modelCallId: 'call-1',
  modelRateSnapshot: [
    { unit: 'llm_input_token', rateId: 'rate-in', rateRevision: 2 },
    { unit: 'llm_output_token', rateId: 'rate-out', rateRevision: 3 },
  ],
  unknownSince: '2026-10-12T09:00:00.000Z', reconciliationDueAt: '2026-10-12T10:00:00.000Z',
}
const buildHold = {
  id: 'hold-build', projectId: 'project-a', amount: '0.600000', remaining: '0.600000', state: 'pending', measurementUnknown: false,
  businessKey: 'build', ownerClientId: 'agent-service', createdAt: '2026-10-12T09:00:00.000Z', buildRequestId: 'build-1', buildAttempt: 2,
}
const buildRecord = {
  attempt: 2, buildRequestId: 'build-1', executionAuthorized: true, executionDeadline: '2026-10-12T09:30:00.000Z', hold: buildHold,
  estimate: { projectId: 'project-a', input: { builderBinding: 'buildkit-primary-v1', from: '2026-10-12T09:00:00.000Z', maxDurationMilliseconds: 1_800_000 },
    until: '2026-10-12T09:30:00.000Z', total: { currency: 'CREDITS', amount: '0.600000' }, lines: [] },
}
function settleEntry(id: string, spent: string, absorbed: string) {
  return { ...holdEntry, id, kind: 'settle', projectId: 'project-a', holdId: 'hold-model', reason: 'measured cost',
    reservedDelta: `-${spent}`, balanceDelta: `-${spent}`, spentDelta: spent, platformAbsorbed: absorbed }
}

const mounted: Array<{ unmount: () => void }> = []
async function mountPanel() {
  const wrapper = mount(ProjectCreditsPanel, { props: { projectId: 'project-a' }, attachTo: document.body })
  mounted.push(wrapper)
  await flushPromises()
  return wrapper
}

function confirmButton() {
  return document.body.querySelector<HTMLButtonElement>('[role="alertdialog"] .filled-button')
}

describe('ProjectCreditsPanel', () => {
  afterEach(() => { for (const wrapper of mounted.splice(0)) wrapper.unmount() })
  beforeEach(() => {
    window.sessionStorage.clear()
    authState.user.value = { profile: { roles: ['admin'], actor_id: 'actor-admin' } }
    vi.mocked(getProjectCredits).mockReset().mockResolvedValue({ data: account, error: undefined } as never)
    vi.mocked(listProjectCreditLedger).mockReset().mockResolvedValue({ data: { items: [grantEntry, holdEntry], nextCursor: null }, error: undefined } as never)
    vi.mocked(listProjectCreditHolds).mockReset().mockResolvedValue({ data: { items: [], nextCursor: null }, error: undefined } as never)
    vi.mocked(createCreditAdminOperation).mockReset().mockResolvedValue({ data: [], error: undefined } as never)
    vi.mocked(getProjectBuildCost).mockReset()
  })

  it('never shows an unverified usage as zero', async () => {
    vi.mocked(getProjectCredits).mockResolvedValue({ data: { ...account, unknown: '0.000000', unknownCount: 2 }, error: undefined } as never)
    const wrapper = await mountPanel()
    const unknown = wrapper.get('[data-testid="credits-unknown"]').text()
    expect(unknown).toContain('2 笔，预占已释放')
    expect(unknown).not.toContain('0.000000')
    expect(wrapper.get('[data-testid="credits-unknown-note"]').text()).toContain('不按零计算')
  })

  it('shows the remaining unknown cap while a hold is still reserved', async () => {
    vi.mocked(getProjectCredits).mockResolvedValue({ data: { ...account, unknown: '3.500000', unknownCount: 1 }, error: undefined } as never)
    const wrapper = await mountPanel()
    expect(wrapper.get('[data-testid="credits-unknown"]').text()).toContain('1 笔，最多 3.500000 CREDITS')
  })

  it('submits a grant without a confirmation and pads the amount', async () => {
    const wrapper = await mountPanel()
    const form = wrapper.get('[data-testid="credits-admin-form"]')
    await form.get('input[aria-label="Credits 金额"]').setValue('20')
    await form.get('textarea[aria-label="操作原因"]').setValue('课程额度')
    await form.trigger('submit')
    await flushPromises()
    expect(confirmButton()).toBeNull()
    expect(createCreditAdminOperation).toHaveBeenCalledWith(expect.objectContaining({
      body: { action: 'grant', projectId: 'project-a', amount: '20.000000', reason: '课程额度' },
    }))
  })

  it('confirms a transfer before calling the server and names the destination project', async () => {
    const wrapper = await mountPanel()
    const form = wrapper.get('[data-testid="credits-admin-form"]')
    await form.get('select[aria-label="Credits 操作"]').setValue('transfer')
    const destination = form.get('select[aria-label="目标项目"]')
    expect(destination.findAll('option').map((option) => option.text())).not.toContain('项目 A')
    await destination.setValue('project-b')
    await form.get('input[aria-label="Credits 金额"]').setValue('5')
    await form.get('textarea[aria-label="操作原因"]').setValue('调拨')
    await form.trigger('submit')
    await flushPromises()
    expect(createCreditAdminOperation).not.toHaveBeenCalled()
    expect(document.body.querySelector('[role="alertdialog"]')?.textContent).toContain('项目 B')
    confirmButton()!.click()
    await flushPromises()
    expect(createCreditAdminOperation).toHaveBeenCalledWith(expect.objectContaining({
      body: { action: 'transfer', projectId: 'project-a', destinationProjectId: 'project-b', amount: '5.000000', reason: '调拨' },
    }))
  })

  it('confirms a negative adjustment but not a positive one', async () => {
    const wrapper = await mountPanel()
    const form = wrapper.get('[data-testid="credits-admin-form"]')
    await form.get('select[aria-label="Credits 操作"]').setValue('adjust')
    await form.get('input[aria-label="Credits 金额"]').setValue('-2')
    await form.get('textarea[aria-label="操作原因"]').setValue('多发更正')
    await form.trigger('submit')
    await flushPromises()
    expect(createCreditAdminOperation).not.toHaveBeenCalled()
    expect(confirmButton()).not.toBeNull()
  })

  it('offers reversal only for reversible entries and confirms it', async () => {
    const wrapper = await mountPanel()
    const rows = wrapper.findAll('[aria-label="Credits 账目"] li')
    expect(rows[0]!.text()).toContain('管理员发放')
    expect(rows[0]!.find('button').exists()).toBe(true)
    expect(rows[1]!.text()).toContain('预占')
    expect(rows[1]!.find('button').exists()).toBe(false)
    await rows[0]!.get('button').trigger('click')
    const form = wrapper.get('[data-testid="credits-admin-form"]')
    expect((form.get('input[aria-label="原账目条目 ID"]').element as HTMLInputElement).value).toBe('entry-grant')
    await form.get('textarea[aria-label="操作原因"]').setValue('发错项目')
    await form.trigger('submit')
    await flushPromises()
    confirmButton()!.click()
    await flushPromises()
    expect(createCreditAdminOperation).toHaveBeenCalledWith(expect.objectContaining({
      body: { action: 'reverse', entryId: 'entry-grant', reason: '发错项目' },
    }))
  })

  it('retries a failed operation with the same body from the diagnostic banner', async () => {
    vi.mocked(createCreditAdminOperation)
      .mockResolvedValueOnce({ data: undefined, error: { type: 'about:blank', title: 'Unavailable', status: 503, detail: 'db', diagnosticCode: 'LW_CREDITS_STORAGE', requestId: 'req-503', retryable: true } } as never)
      .mockResolvedValueOnce({ data: [], error: undefined } as never)
    const wrapper = await mountPanel()
    const form = wrapper.get('[data-testid="credits-admin-form"]')
    await form.get('input[aria-label="Credits 金额"]').setValue('1')
    await form.get('textarea[aria-label="操作原因"]').setValue('补发')
    await form.trigger('submit')
    await flushPromises()
    const banner = wrapper.get('.diagnostic-banner')
    expect(banner.get('.diagnostic-message').text()).toContain('Credits 账本暂时无法写入或读取')
    expect(banner.get('.diagnostic-details').text()).toContain('req-503')
    await banner.get('.diagnostic-retry').trigger('click')
    await flushPromises()
    const calls = vi.mocked(createCreditAdminOperation).mock.calls
    expect(calls).toHaveLength(2)
    expect(calls[1]![0].body).toEqual(calls[0]![0].body)
    expect(calls[1]![0].headers).toEqual(calls[0]![0].headers)
  })

  it('hides every write control from an ordinary member', async () => {
    authState.user.value = { profile: { roles: ['teacher'] } }
    const wrapper = await mountPanel()
    expect(wrapper.find('[data-testid="credits-admin-form"]').exists()).toBe(false)
    expect(wrapper.findAll('button').some((button) => button.text() === '冲正此条')).toBe(false)
    expect(wrapper.get('[data-testid="credits-read-only-note"]').exists()).toBe(true)
  })

  it('sends one POST when the draft is submitted again while the accepted result is still refreshing', async () => {
    const wrapper = await mountPanel()
    let releaseRefresh!: () => void
    vi.mocked(getProjectCredits).mockImplementationOnce(() => new Promise<void>((resolve) => { releaseRefresh = resolve })
      .then(() => ({ data: account, error: undefined })) as never)
    const form = wrapper.get('[data-testid="credits-admin-form"]')
    await form.get('input[aria-label="Credits 金额"]').setValue('6')
    await form.get('textarea[aria-label="操作原因"]').setValue('补发')
    await form.trigger('submit')
    await flushPromises()

    expect(form.get('button[type="submit"]').attributes('disabled')).toBeDefined()
    await form.trigger('submit')
    await form.trigger('submit')
    await flushPromises()
    expect(createCreditAdminOperation).toHaveBeenCalledTimes(1)

    releaseRefresh()
    await flushPromises()
    expect((form.get('input[aria-label="Credits 金额"]').element as HTMLInputElement).value).toBe('')
    expect(createCreditAdminOperation).toHaveBeenCalledTimes(1)
  })

  it.each([
    ['succeeds', { data: [], error: undefined }],
    ['fails', { data: undefined, error: { type: 'about:blank', title: 'Unavailable', status: 503, detail: 'db', diagnosticCode: 'LW_CREDITS_STORAGE', requestId: 'req-late', retryable: true } }],
  ])('keeps the other project\'s draft and banner when a late answer %s', async (_label, answer) => {
    let release!: (value: unknown) => void
    vi.mocked(createCreditAdminOperation).mockReturnValueOnce(new Promise((resolve) => { release = resolve }) as never)
    const wrapper = await mountPanel()
    const form = wrapper.get('[data-testid="credits-admin-form"]')
    await form.get('input[aria-label="Credits 金额"]').setValue('7')
    await form.get('textarea[aria-label="操作原因"]').setValue('A 项目额度')
    await form.trigger('submit')
    await flushPromises()

    await wrapper.setProps({ projectId: 'project-b' })
    await flushPromises()
    const draftForm = wrapper.get('[data-testid="credits-admin-form"]')
    await draftForm.get('input[aria-label="Credits 金额"]').setValue('9')
    await draftForm.get('textarea[aria-label="操作原因"]').setValue('B 项目草稿')
    release(answer)
    await flushPromises()

    expect(wrapper.find('.diagnostic-banner').exists()).toBe(false)
    expect((draftForm.get('input[aria-label="Credits 金额"]').element as HTMLInputElement).value).toBe('9')
    expect((draftForm.get('textarea[aria-label="操作原因"]').element as HTMLTextAreaElement).value).toBe('B 项目草稿')
    expect(draftForm.get('button[type="submit"]').attributes('disabled')).toBeUndefined()
  })

  it('lists an unconfirmed operation after a remount and replays it with the original key', async () => {
    vi.mocked(createCreditAdminOperation)
      .mockRejectedValueOnce(new Error('network lost'))
      .mockResolvedValueOnce({ data: [], error: undefined } as never)
    const first = await mountPanel()
    const form = first.get('[data-testid="credits-admin-form"]')
    await form.get('input[aria-label="Credits 金额"]').setValue('4')
    await form.get('textarea[aria-label="操作原因"]').setValue('补发')
    await form.trigger('submit')
    await flushPromises()
    first.unmount()

    const second = await mountPanel()
    const pending = second.get('[data-testid="credits-pending-operations"]')
    expect(pending.text()).toContain('发放 4.000000 CREDITS · 补发')
    await pending.get('button').trigger('click')
    await flushPromises()
    const calls = vi.mocked(createCreditAdminOperation).mock.calls
    expect(calls[1]![0].headers).toEqual(calls[0]![0].headers)
    expect(second.find('[data-testid="credits-pending-operations"]').exists()).toBe(false)
  })

  it('shows an in-flight model call as unknown usage with its finite deadline and rate snapshot, never as zero', async () => {
    vi.mocked(listProjectCreditHolds).mockResolvedValue({ data: { items: [modelHold], nextCursor: null }, error: undefined } as never)
    const wrapper = await mountPanel()
    const hold = wrapper.get('[data-hold-id="hold-model"]')
    expect(hold.text()).toContain('用量待核实 · 模型调用')
    expect(hold.text()).toContain('预占上限 2.000000 CREDITS · 剩余 2.000000 CREDITS')
    expect(hold.get('[data-testid="credit-hold-model-rates"]').text()).toBe('按费率快照计价：模型输入 token 版本 2、模型输出 token 版本 3')
    const unknown = hold.get('[data-testid="credit-hold-unknown"]').text()
    expect(unknown).toContain('不按零计算')
    expect(unknown).toContain('模型调用进行中或尚未上报 token 用量')
    expect(unknown).toContain('核实截止')
    expect(unknown).toContain('到期后释放剩余预占')
    expect(hold.find('[data-testid="credit-hold-settlement"]').exists()).toBe(false)
  })

  it('shows an expired unknown hold as released funds with usage still unverified', async () => {
    vi.mocked(listProjectCreditHolds).mockResolvedValue({ data: { items: [{ ...modelHold, state: 'released', remaining: '0.000000', expiredUnknownAt: '2026-10-12T10:00:00.000Z' }], nextCursor: null }, error: undefined } as never)
    const wrapper = await mountPanel()
    const hold = wrapper.get('[data-hold-id="hold-model"]')
    expect(hold.text()).toContain('待核实，预占已释放')
    expect(hold.get('[data-testid="credit-hold-unknown"]').text()).toContain('之后确认的费用由平台承担')
  })

  it('sums a model hold\'s settlement and platform-borne tail from linked ledger entries', async () => {
    vi.mocked(listProjectCreditHolds).mockResolvedValue({ data: { items: [{ ...modelHold, state: 'settled', measurementUnknown: false, remaining: '0.000000' }], nextCursor: null }, error: undefined } as never)
    vi.mocked(listProjectCreditLedger).mockResolvedValue({ data: { items: [
      settleEntry('entry-settle-1', '1.500000', '0.000000'),
      settleEntry('entry-settle-2', '0.500000', '0.250001'),
      { ...settleEntry('entry-platform', '0.000000', '0.000000'), projectId: null, balanceDelta: '2.000000' },
      grantEntry,
    ], nextCursor: null }, error: undefined } as never)
    const wrapper = await mountPanel()
    const hold = wrapper.get('[data-hold-id="hold-model"]')
    expect(hold.text()).toContain('已结算 · 模型调用')
    expect(hold.get('[data-testid="credit-hold-settlement"]').text()).toContain('扣收 2.000000 CREDITS · 平台承担 0.250001 CREDITS')
    const ledgerRows = wrapper.findAll('[aria-label="Credits 账目"] li')
    expect(ledgerRows[0]!.text()).toContain('关联预占：模型调用')
    expect(ledgerRows[3]!.text()).not.toContain('关联预占')
  })

  it('does not present a settled hold without loaded settlement entries as a zero charge', async () => {
    vi.mocked(listProjectCreditHolds).mockResolvedValue({ data: { items: [{ ...modelHold, state: 'settled', measurementUnknown: false, remaining: '0.000000' }], nextCursor: null }, error: undefined } as never)
    const wrapper = await mountPanel()
    const settlement = wrapper.get('[data-hold-id="hold-model"] [data-testid="credit-hold-settlement"]').text()
    expect(settlement).toContain('不在当前账目页')
    expect(settlement).not.toContain('0.000000')
  })

  it('reads a build hold\'s cost record on demand and keeps unknown build time unpriced', async () => {
    vi.mocked(listProjectCreditHolds).mockResolvedValue({ data: { items: [buildHold], nextCursor: null }, error: undefined } as never)
    vi.mocked(getProjectBuildCost).mockResolvedValue({ data: {
      ...buildRecord, measurement: { state: 'unknown', reason: 'BuildKit history unavailable' }, firstUnknownObservation: { state: 'unknown', reason: 'BuildKit history unavailable' },
    }, error: undefined } as never)
    const wrapper = await mountPanel()
    const hold = wrapper.get('[data-hold-id="hold-build"]')
    expect(hold.text()).toContain('预占中 · 镜像构建（第 2 次）')
    expect(getProjectBuildCost).not.toHaveBeenCalled()
    await hold.get('[data-testid="credit-hold-build-cost-toggle"]').trigger('click')
    await flushPromises()
    expect(getProjectBuildCost).toHaveBeenCalledWith({ path: { projectId: 'project-a', buildRequestId: 'build-1', attempt: 2 } })
    const record = hold.get('[data-testid="build-cost-record"]')
    expect(record.get('[data-testid="build-cost-estimate"]').text()).toContain('0.600000 CREDITS（构建器 buildkit-primary-v1，最长 30 分钟）')
    expect(record.get('[data-testid="build-cost-measurement"]').text()).toBe('构建用时未知，待核实：BuildKit history unavailable')
    expect(record.get('[data-testid="build-cost-uncharged"]').text()).toContain('实际费用未知，不按零计算')
    expect(record.find('[data-testid="build-cost-charged"]').exists()).toBe(false)
  })

  it('shows a settled build charge with its rate lines', async () => {
    vi.mocked(listProjectCreditHolds).mockResolvedValue({ data: { items: [{ ...buildHold, state: 'settled', remaining: '0.000000' }], nextCursor: null }, error: undefined } as never)
    vi.mocked(getProjectBuildCost).mockResolvedValue({ data: {
      ...buildRecord,
      measurement: { state: 'known', startedAt: '2026-10-12T09:00:00.000Z', finishedAt: '2026-10-12T09:10:00.000Z', historyRef: 'ref-1' },
      chargedTotal: { currency: 'CREDITS', amount: '0.200000' },
      chargedLines: [{ rateId: 'rate-build', rateRevision: 4, unit: 'build_second', quantity: 600_000, quantityDivisor: 1000, unitQuantity: 3600,
        unitPrice: { currency: 'CREDITS', amount: '1.200000' }, amount: { currency: 'CREDITS', amount: '0.200000' } }],
    }, error: undefined } as never)
    const wrapper = await mountPanel()
    await wrapper.get('[data-testid="credit-hold-build-cost-toggle"]').trigger('click')
    await flushPromises()
    const record = wrapper.get('[data-testid="build-cost-record"]')
    expect(record.get('[data-testid="build-cost-charged"]').text()).toBe('0.200000 CREDITS')
    expect(record.text()).toContain('费率版本 4 · 600000 毫秒 · 单价 1.200000 CREDITS / 3600 秒 · 0.200000 CREDITS')
  })

  it('keeps the request ID and Chinese reason when the build cost record cannot be read', async () => {
    vi.mocked(listProjectCreditHolds).mockResolvedValue({ data: { items: [buildHold], nextCursor: null }, error: undefined } as never)
    vi.mocked(getProjectBuildCost).mockResolvedValue({ data: undefined, error: {
      type: 'about:blank', title: 'Conflict', status: 409, detail: 'overflow', diagnosticCode: 'LW_RESOURCE_NUMERIC_OVERFLOW', requestId: 'req-build', retryable: false,
    } } as never)
    const wrapper = await mountPanel()
    await wrapper.get('[data-testid="credit-hold-build-cost-toggle"]').trigger('click')
    await flushPromises()
    const banner = wrapper.get('[data-testid="build-cost-record"] .diagnostic-banner')
    expect(banner.get('.diagnostic-message').text()).toContain('超出平台可精确计算的范围')
    expect(banner.get('.diagnostic-details').text()).toContain('req-build')
  })
})
