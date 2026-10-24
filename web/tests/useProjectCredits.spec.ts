import { flushPromises } from '@vue/test-utils'
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { ref } from 'vue'
import {
  createCreditAdminOperation,
  getProjectCredits,
  listProjectCreditHolds,
  listProjectCreditLedger,
} from '@/generated/contracts'
import {
  canonicalCreditsAmount,
  creditHoldStateLabel,
  creditLedgerKindLabel,
  isDestructiveCreditOperation,
  isPositiveCreditsAmount,
  useProjectCredits,
} from '@/composables/useProjectCredits'

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

const account = {
  projectId: 'project-new',
  currency: 'CREDITS',
  balance: '20.000000',
  available: '15.000000',
  reserved: '5.000000',
  spent: '3.000000',
  pending: '5.000000',
  unknown: '0.000000',
  unknownCount: 0,
  platformAbsorbed: '0.000000',
  revision: 2,
}
const emptyPage = { data: { items: [], page: 1, pageSize: 25, hasMore: false }, error: undefined }
const grant = { action: 'grant' as const, amount: '10.000000', reason: '补发额度', destinationProjectId: '', entryId: '' }

function mockReads() {
  vi.mocked(getProjectCredits).mockResolvedValue({ data: account, error: undefined } as never)
  vi.mocked(listProjectCreditLedger).mockResolvedValue(emptyPage as never)
  vi.mocked(listProjectCreditHolds).mockResolvedValue(emptyPage as never)
}

describe('Credits amount and label helpers', () => {
  it('pads typed amounts to six decimals without rounding and rejects disallowed signs', () => {
    expect(canonicalCreditsAmount('20', false)).toBe('20.000000')
    expect(canonicalCreditsAmount(' 0.5 ', false)).toBe('0.500000')
    expect(canonicalCreditsAmount('-5.25', true)).toBe('-5.250000')
    expect(canonicalCreditsAmount('-5', false)).toBeNull()
    expect(canonicalCreditsAmount('1.0000001', false)).toBeNull()
    expect(canonicalCreditsAmount('1e3', false)).toBeNull()
    expect(isPositiveCreditsAmount('20')).toBe(false)
    expect(isPositiveCreditsAmount('20.000000')).toBe(true)
    expect(isPositiveCreditsAmount('0.000000')).toBe(false)
  })

  it('treats transfer, reverse and negative adjust as destructive; grant and positive adjust are not', () => {
    expect(isDestructiveCreditOperation({ action: 'grant', amount: '1.000000' })).toBe(false)
    expect(isDestructiveCreditOperation({ action: 'adjust', amount: '1.000000' })).toBe(false)
    expect(isDestructiveCreditOperation({ action: 'adjust', amount: '-1.000000' })).toBe(true)
    expect(isDestructiveCreditOperation({ action: 'transfer', amount: '1.000000' })).toBe(true)
    expect(isDestructiveCreditOperation({ action: 'reverse', amount: '' })).toBe(true)
  })

  it('labels known server kinds and states, and shows unknown ones verbatim', () => {
    expect(creditLedgerKindLabel('settle')).toBe('扣收')
    expect(creditLedgerKindLabel('new_kind')).toBe('new_kind')
    expect(creditHoldStateLabel('unknown')).toBe('用量待核实')
    expect(creditHoldStateLabel('later')).toContain('later')
  })
})

describe('useProjectCredits', () => {
  beforeEach(() => {
    vi.mocked(getProjectCredits).mockReset()
    vi.mocked(listProjectCreditLedger).mockReset()
    vi.mocked(listProjectCreditHolds).mockReset()
    vi.mocked(createCreditAdminOperation).mockReset()
  })

  it('loads account, ledger and holds for the selected project', async () => {
    mockReads()
    vi.mocked(listProjectCreditLedger).mockResolvedValue({
      data: {
        items: [{
          id: 'entry-1', kind: 'grant', operationId: 'op-1', reason: '课程额度', balanceDelta: '20.000000', reservedDelta: '0.000000',
          spentDelta: '0.000000', platformAbsorbed: '0.000000', createdAt: '2026-10-12T08:00:00.000Z', rateSnapshots: [],
        }],
        page: 1,
        pageSize: 25,
        hasMore: false,
      },
      error: undefined,
    } as never)

    const credits = useProjectCredits(ref<string | null>('project-new'))
    await flushPromises()

    expect(getProjectCredits).toHaveBeenCalledWith({ path: { projectId: 'project-new' } })
    expect(credits.account.value.kind).toBe('success')
    expect(credits.ledger.value.kind).toBe('success')
    expect(credits.holds.value.kind).toBe('empty')
  })

  it('keeps the request ID and Chinese reason of a denied read', async () => {
    mockReads()
    vi.mocked(getProjectCredits).mockResolvedValue({
      data: undefined,
      error: { type: 'about:blank', title: 'Forbidden', status: 403, detail: 'scope denied', diagnosticCode: 'LW_CREDITS_SCOPE_DENIED', requestId: 'req-403', retryable: false },
    } as never)
    const credits = useProjectCredits(ref<string | null>('project-new'))
    await flushPromises()
    expect(credits.account.value.kind).toBe('error')
    if (credits.account.value.kind === 'error') {
      expect(credits.account.value.diagnostic.message).toContain('无权查看或修改这个项目的 Credits')
      expect(credits.account.value.diagnostic.requestId).toBe('req-403')
    }
  })

  it('submits grant with an idempotency key and reloads', async () => {
    mockReads()
    vi.mocked(createCreditAdminOperation).mockResolvedValue({ data: [], error: undefined } as never)
    const credits = useProjectCredits(ref<string | null>('project-new'))
    await flushPromises()
    expect(await credits.submitAdminOperation(grant)).toBe(true)
    expect(createCreditAdminOperation).toHaveBeenCalledWith({
      headers: { 'Idempotency-Key': expect.any(String) },
      body: { action: 'grant', projectId: 'project-new', amount: '10.000000', reason: '补发额度' },
    })
    expect(getProjectCredits).toHaveBeenCalledTimes(2)
    expect(credits.outcome.value?.kind).toBe('success')
  })

  it('replays a failed operation with the same key and uses a new key after success', async () => {
    mockReads()
    vi.mocked(createCreditAdminOperation)
      .mockRejectedValueOnce(new Error('network lost'))
      .mockResolvedValueOnce({ data: [], error: undefined } as never)
      .mockResolvedValueOnce({ data: [], error: undefined } as never)
    const credits = useProjectCredits(ref<string | null>('project-new'))
    await flushPromises()

    expect(await credits.submitAdminOperation(grant)).toBe(false)
    expect(credits.outcome.value?.diagnostic.message).toContain('不会重复入账')
    expect(await credits.submitAdminOperation(grant)).toBe(true)
    expect(await credits.submitAdminOperation(grant)).toBe(true)

    const keys = vi.mocked(createCreditAdminOperation).mock.calls.map(([options]) => (options.headers as Record<string, string>)['Idempotency-Key'])
    expect(keys[0]).toBe(keys[1])
    expect(keys[2]).not.toBe(keys[1])
  })

  it('shows insufficient balance as a Chinese reason with a next step', async () => {
    mockReads()
    vi.mocked(createCreditAdminOperation).mockResolvedValue({
      data: undefined,
      error: { type: 'about:blank', title: 'Conflict', status: 409, detail: 'insufficient', diagnosticCode: 'LW_CREDITS_INSUFFICIENT', requestId: 'req-409', retryable: false },
    } as never)
    const credits = useProjectCredits(ref<string | null>('project-new'))
    await flushPromises()
    expect(await credits.submitAdminOperation({ ...grant, action: 'transfer', destinationProjectId: 'project-other' })).toBe(false)
    expect(credits.outcome.value?.diagnostic).toMatchObject({
      code: 'LW_CREDITS_INSUFFICIENT',
      requestId: 'req-409',
    })
    expect(credits.outcome.value?.diagnostic.message).toContain('可用余额不足')
    expect(credits.outcome.value?.diagnostic.nextStep).toContain('发放 Credits')
  })

  it('rejects invalid operations locally without calling the server', async () => {
    mockReads()
    const credits = useProjectCredits(ref<string | null>('project-new'))
    await flushPromises()
    expect(await credits.submitAdminOperation({ ...grant, reason: '  ' })).toBe(false)
    expect(await credits.submitAdminOperation({ ...grant, action: 'transfer', destinationProjectId: 'project-new' })).toBe(false)
    expect(await credits.submitAdminOperation({ ...grant, action: 'adjust', amount: '0.000000' })).toBe(false)
    expect(await credits.submitAdminOperation({ ...grant, action: 'reverse', entryId: '' })).toBe(false)
    expect(createCreditAdminOperation).not.toHaveBeenCalled()
  })
})
