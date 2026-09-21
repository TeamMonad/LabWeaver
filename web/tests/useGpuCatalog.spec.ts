import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useGpuCatalog } from '@/composables/useGpuCatalog'
import {
  createResourceGpuCatalogEntry,
  listResourceGpuCatalog,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    listResourceGpuCatalog: vi.fn(),
    createResourceGpuCatalogEntry: vi.fn(),
  }
})

const entry = {
  id: '0197f0e0-0000-7000-8000-0000000000a1',
  class: 'nvidia-a10',
  mode: 'exclusive' as const,
  providerBinding: 'kubernetes',
  capacityUnits: 4,
  allocationBinding: 'nvidia.com/gpu',
  revision: 2,
  active: true,
}

function problem(diagnosticCode: string, detail: string) {
  return { diagnosticCode, detail, retryable: false }
}

describe('useGpuCatalog', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    vi.mocked(listResourceGpuCatalog).mockResolvedValue({ data: [entry], error: undefined as never })
  })

  it('renders the server catalog projection after load', async () => {
    const catalog = useGpuCatalog()

    await catalog.load()

    expect(listResourceGpuCatalog).toHaveBeenCalledWith()
    expect(catalog.state).toEqual({ kind: 'ready' })
    expect(catalog.entries).toEqual([entry])
  })

  it('creates a client-identified active revision and reloads the catalog', async () => {
    vi.mocked(createResourceGpuCatalogEntry).mockResolvedValue({
      data: { ...entry, id: '0197f0e0-0000-7000-8000-0000000000a3', revision: 3 },
      error: undefined as never,
    })
    const catalog = useGpuCatalog()

    await expect(catalog.create({
      class: 'nvidia-a10',
      mode: 'exclusive',
      providerBinding: 'kubernetes',
      capacityUnits: 8,
      allocationBinding: 'nvidia.com/gpu',
      revision: 3,
    })).resolves.toBe(true)

    const request = vi.mocked(createResourceGpuCatalogEntry).mock.calls[0][0]
    expect(request.headers).toEqual({ 'Idempotency-Key': expect.any(String) })
    expect(request.body).toEqual({
      id: expect.stringMatching(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-8[0-9a-f]{3}-[0-9a-f]{12}$/),
      class: 'nvidia-a10',
      mode: 'exclusive',
      providerBinding: 'kubernetes',
      capacityUnits: 8,
      allocationBinding: 'nvidia.com/gpu',
      revision: 3,
      active: true,
    })
    expect(listResourceGpuCatalog).toHaveBeenCalledTimes(1)
    expect(catalog.entries).toEqual([entry])
  })

  it('surfaces an authorization failure and keeps the loaded catalog', async () => {
    vi.mocked(createResourceGpuCatalogEntry).mockResolvedValue({
      error: problem('LW_ACCESS_PERMISSION_DENIED', '当前身份没有维护 GPU 目录的权限。'),
    } as never)
    const catalog = useGpuCatalog()
    await catalog.load()

    await expect(catalog.create({
      class: 'nvidia-a10',
      mode: 'exclusive',
      providerBinding: 'kubernetes',
      capacityUnits: 8,
      allocationBinding: 'nvidia.com/gpu',
      revision: 3,
    })).resolves.toBe(false)

    expect(catalog.entries).toEqual([entry])
    expect(catalog.state).toEqual({
      kind: 'error',
      diagnostic: { code: 'LW_ACCESS_PERMISSION_DENIED', message: '当前身份没有维护 GPU 目录的权限。', retryable: false },
    })
    expect(listResourceGpuCatalog).toHaveBeenCalledTimes(1)

    catalog.clearDiagnostic()
    expect(catalog.state).toEqual({ kind: 'ready' })
  })
})
