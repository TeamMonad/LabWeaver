import { flushPromises } from '@vue/test-utils'
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { ref } from 'vue'
import {
  createCreditAdminOperation,
  getProjectCredits,
  listProjectCreditHolds,
  listProjectCreditLedger,
} from '@/generated/contracts'
import { isPositiveCreditsAmount, useProjectCredits } from '@/composables/useProjectCredits'

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

describe('useProjectCredits', () => {
  beforeEach(() => {
    vi.mocked(getProjectCredits).mockReset()
    vi.mocked(listProjectCreditLedger).mockReset()
    vi.mocked(listProjectCreditHolds).mockReset()
    vi.mocked(createCreditAdminOperation).mockReset()
  })

  it('rejects non six-decimal positive amounts', () => {
    expect(isPositiveCreditsAmount('20')).toBe(false)
    expect(isPositiveCreditsAmount('20.000000')).toBe(true)
    expect(isPositiveCreditsAmount('0.000000')).toBe(false)
  })

  it('loads account, ledger and holds for the selected project', async () => {
    vi.mocked(getProjectCredits).mockResolvedValue({ data: account, error: undefined } as never)
    vi.mocked(listProjectCreditLedger).mockResolvedValue({
      data: {
        items: [{
          id: 'entry-1',
          kind: 'grant',
          operationId: 'op-1',
          reason: '课程额度',
          balanceDelta: '20.000000',
          reservedDelta: '0.000000',
          spentDelta: '0.000000',
          platformAbsorbed: '0.000000',
          createdAt: '2026-10-12T08:00:00.000Z',
        }],
        page: 1,
        pageSize: 25,
        hasMore: false,
      },
      error: undefined,
    } as never)
    vi.mocked(listProjectCreditHolds).mockResolvedValue({
      data: { items: [], page: 1, pageSize: 25, hasMore: false },
      error: undefined,
    } as never)

    const projectId = ref<string | null>('project-new')
    const credits = useProjectCredits(projectId)
    await flushPromises()

    expect(getProjectCredits).toHaveBeenCalledWith({ path: { projectId: 'project-new' } })
    expect(credits.account.value.kind).toBe('success')
    if (credits.account.value.kind === 'success') {
      expect(credits.account.value.data.available).toBe('15.000000')
    }
    expect(credits.ledger.value.kind).toBe('success')
    expect(credits.holds.value.kind).toBe('empty')
  })

  it('submits grant with idempotency key and reloads', async () => {
    vi.mocked(getProjectCredits).mockResolvedValue({ data: account, error: undefined } as never)
    vi.mocked(listProjectCreditLedger).mockResolvedValue({
      data: { items: [], page: 1, pageSize: 25, hasMore: false },
      error: undefined,
    } as never)
    vi.mocked(listProjectCreditHolds).mockResolvedValue({
      data: { items: [], page: 1, pageSize: 25, hasMore: false },
      error: undefined,
    } as never)
    vi.mocked(createCreditAdminOperation).mockResolvedValue({
      data: [],
      error: undefined,
    } as never)

    const projectId = ref<string | null>('project-new')
    const credits = useProjectCredits(projectId)
    await flushPromises()
    const ok = await credits.submitAdminOperation({
      action: 'grant',
      amount: '10.000000',
      reason: '补发额度',
      destinationProjectId: '',
      entryId: '',
    })
    expect(ok).toBe(true)
    expect(createCreditAdminOperation).toHaveBeenCalledWith(expect.objectContaining({
      headers: expect.objectContaining({ 'Idempotency-Key': expect.any(String) }),
      body: {
        action: 'grant',
        projectId: 'project-new',
        amount: '10.000000',
        reason: '补发额度',
      },
    }))
    expect(credits.outcome.value?.kind).toBe('success')
  })
})
