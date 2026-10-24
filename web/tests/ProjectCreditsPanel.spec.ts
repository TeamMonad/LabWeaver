import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'
import ProjectCreditsPanel from '@/components/console/ProjectCreditsPanel.vue'
import {
  createCreditAdminOperation,
  getProjectCredits,
  listProjectCreditHolds,
  listProjectCreditLedger,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    createCreditAdminOperation: vi.fn(),
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
    vi.mocked(listProjectCreditLedger).mockReset().mockResolvedValue({ data: { items: [grantEntry, holdEntry], page: 1, pageSize: 25, hasMore: false }, error: undefined } as never)
    vi.mocked(listProjectCreditHolds).mockReset().mockResolvedValue({ data: { items: [], page: 1, pageSize: 25, hasMore: false }, error: undefined } as never)
    vi.mocked(createCreditAdminOperation).mockReset().mockResolvedValue({ data: [], error: undefined } as never)
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
})
