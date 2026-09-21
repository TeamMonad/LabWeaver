import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import GpuCatalogView from '@/views/admin/GpuCatalogView.vue'
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

const activeEntry = {
  id: '0197f0e0-0000-7000-8000-0000000000a1',
  class: 'nvidia-a10',
  mode: 'exclusive' as const,
  providerBinding: 'kubernetes',
  capacityUnits: 4,
  allocationBinding: 'nvidia.com/gpu',
  revision: 2,
  active: true,
}

const retiredEntry = { ...activeEntry, id: '0197f0e0-0000-7000-8000-0000000000a2', revision: 1, active: false }

const mounted: VueWrapper[] = []

async function mountView() {
  const wrapper = mount(GpuCatalogView)
  mounted.push(wrapper)
  await flushPromises()
  return wrapper
}

function columnText(wrapper: VueWrapper, rowIndex: number, title: string): string {
  const column = wrapper.findAll('.catalog-table thead th').map((header) => header.text()).indexOf(title)
  if (column < 0) throw new Error(`missing column ${title}`)
  return wrapper.findAll('.catalog-table tbody tr')[rowIndex].findAll('td')[column].text()
}

async function fillCreateForm(wrapper: VueWrapper) {
  const form = wrapper.get('.create-card .admin-form')
  await form.get('select').setValue('container_time_slice')
  const inputs = form.findAll('input')
  await inputs[0].setValue('nvidia-h100')
  await inputs[1].setValue('kubernetes')
  await inputs[2].setValue('8')
  await inputs[3].setValue('nvidia.com/gpu')
  await inputs[4].setValue('3')
  return form
}

describe('GpuCatalogView', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    vi.mocked(listResourceGpuCatalog).mockResolvedValue({
      data: [activeEntry, retiredEntry],
      error: undefined as never,
    })
  })

  afterEach(() => {
    for (const wrapper of mounted.splice(0)) wrapper.unmount()
  })

  it('lists every catalog revision with its mode, bindings, capacity and state', async () => {
    const wrapper = await mountView()

    expect(wrapper.text()).toContain('维护 GPU class 目录')
    expect(columnText(wrapper, 0, 'class')).toBe('nvidia-a10')
    expect(columnText(wrapper, 0, 'mode')).toBe('独占')
    expect(columnText(wrapper, 0, 'provider binding')).toBe('kubernetes')
    expect(columnText(wrapper, 0, 'allocation binding')).toBe('nvidia.com/gpu')
    expect(columnText(wrapper, 0, 'capacity units')).toBe('4')
    expect(columnText(wrapper, 0, '版本')).toBe('2')
    expect(columnText(wrapper, 0, '状态')).toBe('可用')
    expect(columnText(wrapper, 1, '状态')).toBe('已停用')
  })

  it('creates an entry with the reviewed body and reloads the catalog', async () => {
    vi.mocked(createResourceGpuCatalogEntry).mockResolvedValue({
      data: { ...activeEntry, class: 'nvidia-h100', mode: 'container_time_slice', capacityUnits: 8, revision: 3 },
      error: undefined as never,
    })
    const wrapper = await mountView()
    const form = await fillCreateForm(wrapper)

    await form.trigger('submit')
    await flushPromises()

    const request = vi.mocked(createResourceGpuCatalogEntry).mock.calls[0][0]
    expect(request.headers).toEqual({ 'Idempotency-Key': expect.any(String) })
    expect(request.body).toEqual({
      id: expect.stringMatching(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-8[0-9a-f]{3}-[0-9a-f]{12}$/),
      class: 'nvidia-h100',
      mode: 'container_time_slice',
      providerBinding: 'kubernetes',
      capacityUnits: 8,
      allocationBinding: 'nvidia.com/gpu',
      revision: 3,
      active: true,
    })
    expect(listResourceGpuCatalog).toHaveBeenCalledTimes(2)
    expect(columnText(wrapper, 0, 'class')).toBe('nvidia-a10')
  })

  it('surfaces the revision conflict and keeps the rendered catalog', async () => {
    vi.mocked(createResourceGpuCatalogEntry).mockResolvedValue({
      error: { diagnosticCode: 'LW_RESOURCE_GPU_CATALOG_REVISION_CONFLICT', detail: '该 class 已有更高或相同的目录版本。' },
    } as never)
    const wrapper = await mountView()
    const form = await fillCreateForm(wrapper)

    await form.trigger('submit')
    await flushPromises()

    const banner = wrapper.get('.diagnostic-banner').text()
    expect(banner).toContain('LW_RESOURCE_GPU_CATALOG_REVISION_CONFLICT')
    expect(banner).toContain('该 class 已有更高或相同的目录版本。')
    expect(columnText(wrapper, 0, 'class')).toBe('nvidia-a10')
    expect(listResourceGpuCatalog).toHaveBeenCalledTimes(1)
  })
})
