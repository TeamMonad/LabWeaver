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

function keyOf(call: number) {
  return (vi.mocked(createCreditAdminOperation).mock.calls[call]![0].headers as Record<string, string>)['Idempotency-Key']
}

function setup(project = 'project-new', actor: string | null = 'actor-admin') {
  const projectId = ref<string | null>(project)
  const actorId = ref<string | null>(actor)
  const credits = useProjectCredits(projectId, actorId)
  return { credits, projectId, actorId }
}

const lostResponse = () => Promise.reject(new Error('network lost'))
const accepted = () => Promise.resolve({ data: [], error: undefined } as never)

describe('useProjectCredits', () => {
  beforeEach(() => {
    window.sessionStorage.clear()
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

    const { credits } = setup()
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
    const { credits } = setup()
    await flushPromises()
    expect(credits.account.value.kind).toBe('error')
    if (credits.account.value.kind === 'error') {
      expect(credits.account.value.diagnostic.message).toContain('无权查看或修改这个项目的 Credits')
      expect(credits.account.value.diagnostic.requestId).toBe('req-403')
    }
  })

  it('submits grant with an idempotency key, reloads and leaves nothing pending', async () => {
    mockReads()
    vi.mocked(createCreditAdminOperation).mockImplementation(accepted)
    const { credits } = setup()
    await flushPromises()
    expect(await credits.submitAdminOperation(grant)).toBe('accepted')
    expect(createCreditAdminOperation).toHaveBeenCalledWith({
      headers: { 'Idempotency-Key': expect.any(String) },
      body: { action: 'grant', projectId: 'project-new', amount: '10.000000', reason: '补发额度' },
    })
    expect(getProjectCredits).toHaveBeenCalledTimes(2)
    expect(credits.outcome.value?.kind).toBe('success')
    expect(credits.pendingOperations.value).toEqual([])
  })

  it('replays a lost operation with the same key and uses a new key after success', async () => {
    mockReads()
    vi.mocked(createCreditAdminOperation).mockImplementationOnce(lostResponse).mockImplementation(accepted)
    const { credits } = setup()
    await flushPromises()

    expect(await credits.submitAdminOperation(grant)).toBe('failed')
    expect(credits.outcome.value?.diagnostic.message).toContain('不会重复入账')
    expect(credits.pendingOperations.value).toHaveLength(1)
    expect(await credits.submitAdminOperation(grant)).toBe('accepted')
    expect(await credits.submitAdminOperation(grant)).toBe('accepted')
    expect(keyOf(0)).toBe(keyOf(1))
    expect(keyOf(2)).not.toBe(keyOf(1))
  })

  it('keeps an unconfirmed key when another draft or another project is submitted in between', async () => {
    mockReads()
    vi.mocked(createCreditAdminOperation).mockImplementationOnce(lostResponse).mockImplementation(accepted)
    const { credits, projectId } = setup('project-a')
    await flushPromises()

    expect(await credits.submitAdminOperation(grant)).toBe('failed')
    expect(await credits.submitAdminOperation({ ...grant, amount: '3.000000' })).toBe('accepted')
    projectId.value = 'project-b'
    await flushPromises()
    expect(credits.pendingOperations.value).toEqual([])
    expect(await credits.submitAdminOperation(grant)).toBe('accepted')
    projectId.value = 'project-a'
    await flushPromises()
    expect(credits.pendingOperations.value.map((item) => item.input.amount)).toEqual(['10.000000'])

    expect(await credits.submitAdminOperation(grant)).toBe('accepted')
    expect(keyOf(3)).toBe(keyOf(0))
    expect(new Set([keyOf(0), keyOf(1), keyOf(2)]).size).toBe(3)
    expect(credits.pendingOperations.value).toEqual([])
  })

  it('restores an unconfirmed operation after a refresh and retries it with the original key', async () => {
    mockReads()
    vi.mocked(createCreditAdminOperation).mockImplementationOnce(lostResponse).mockImplementation(accepted)
    const first = setup()
    await flushPromises()
    expect(await first.credits.submitAdminOperation(grant)).toBe('failed')

    // A new page instance reads the same session storage.
    const refreshed = setup()
    await flushPromises()
    expect(refreshed.credits.pendingOperations.value).toHaveLength(1)
    expect(refreshed.credits.pendingOperations.value[0]!.input).toMatchObject({ action: 'grant', amount: '10.000000', reason: '补发额度' })
    expect(await refreshed.credits.retryPending(refreshed.credits.pendingOperations.value[0]!.scope)).toBe('accepted')
    expect(keyOf(1)).toBe(keyOf(0))
    expect(refreshed.credits.pendingOperations.value).toEqual([])
  })

  it('does not offer another actor\'s unconfirmed operations', async () => {
    mockReads()
    vi.mocked(createCreditAdminOperation).mockImplementationOnce(lostResponse)
    const { credits, actorId } = setup()
    await flushPromises()
    expect(await credits.submitAdminOperation(grant)).toBe('failed')
    actorId.value = 'actor-other'
    await flushPromises()
    expect(credits.pendingOperations.value).toEqual([])
  })

  it('drops the pending entry only when the server proves it was not applied', async () => {
    mockReads()
    vi.mocked(createCreditAdminOperation).mockResolvedValue({
      data: undefined,
      error: { type: 'about:blank', title: 'Conflict', status: 409, detail: 'insufficient', diagnosticCode: 'LW_CREDITS_INSUFFICIENT', requestId: 'req-409', retryable: false },
    } as never)
    const { credits } = setup()
    await flushPromises()
    expect(await credits.submitAdminOperation({ ...grant, action: 'transfer', destinationProjectId: 'project-other' })).toBe('failed')
    expect(credits.outcome.value?.diagnostic).toMatchObject({
      code: 'LW_CREDITS_INSUFFICIENT',
      requestId: 'req-409',
    })
    expect(credits.outcome.value?.diagnostic.message).toContain('可用余额不足')
    expect(credits.outcome.value?.diagnostic.nextStep).toContain('发放 Credits')
    expect(credits.pendingOperations.value).toEqual([])

    vi.mocked(createCreditAdminOperation).mockResolvedValue({
      data: undefined,
      error: { type: 'about:blank', title: 'Unavailable', status: 503, detail: 'db', diagnosticCode: 'LW_CREDITS_STORAGE', requestId: 'req-503', retryable: true },
    } as never)
    expect(await credits.submitAdminOperation(grant)).toBe('failed')
    expect(credits.pendingOperations.value).toHaveLength(1)
  })

  it.each([
    ['succeeds', accepted],
    ['fails', lostResponse],
  ])('ignores an answer that %s after the project changed', async (_label, answer) => {
    mockReads()
    let release!: () => void
    vi.mocked(createCreditAdminOperation).mockImplementation(() => new Promise<void>((resolve) => { release = resolve }).then(answer))
    const { credits, projectId } = setup('project-a')
    await flushPromises()
    const pending = credits.submitAdminOperation(grant)
    await flushPromises()
    expect(credits.acting.value).toBe('grant')

    projectId.value = 'project-b'
    await flushPromises()
    expect(credits.acting.value).toBeNull()
    const readsBefore = vi.mocked(getProjectCredits).mock.calls.length
    release()
    expect(await pending).toBe('stale')
    await flushPromises()

    expect(credits.outcome.value).toBeNull()
    expect(credits.acting.value).toBeNull()
    expect(vi.mocked(getProjectCredits).mock.calls.length).toBe(readsBefore)
    expect(credits.pendingOperations.value).toEqual([])
  })

  it('rejects invalid operations locally without calling the server', async () => {
    mockReads()
    const { credits } = setup()
    await flushPromises()
    expect(await credits.submitAdminOperation({ ...grant, reason: '  ' })).toBe('failed')
    expect(await credits.submitAdminOperation({ ...grant, action: 'transfer', destinationProjectId: 'project-new' })).toBe('failed')
    expect(await credits.submitAdminOperation({ ...grant, action: 'adjust', amount: '0.000000' })).toBe('failed')
    expect(await credits.submitAdminOperation({ ...grant, action: 'reverse', entryId: '' })).toBe('failed')
    expect(createCreditAdminOperation).not.toHaveBeenCalled()
  })
})
