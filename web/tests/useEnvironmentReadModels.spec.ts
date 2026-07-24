import { afterEach, describe, expect, it, vi } from 'vitest'
import { ref } from 'vue'
import { useEnvironmentInstance } from '@/composables/useEnvironmentInstance'
import { useEnvironmentOperations } from '@/composables/useEnvironmentOperations'
import { useEnvironmentTemplateReleases } from '@/composables/useEnvironmentTemplateReleases'
import {
  getEnvironment,
  listEnvironmentOperations,
  listEnvironmentTemplateReleases,
} from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    getEnvironment: vi.fn(),
    listEnvironmentOperations: vi.fn(),
    listEnvironmentTemplateReleases: vi.fn(),
  }
})

afterEach(() => {
  vi.clearAllMocks()
})

describe('environment read-model composables', () => {
  it.each([
    {
      create: () => useEnvironmentTemplateReleases(ref('01900000-0000-7000-8000-000000000001')),
      load: (state: ReturnType<typeof useEnvironmentTemplateReleases>) => state.load(),
      read: (state: ReturnType<typeof useEnvironmentTemplateReleases>) => state.releases,
      client: listEnvironmentTemplateReleases,
      code: 'RELEASE_LIST_RESPONSE_INVALID',
    },
    {
      create: () => useEnvironmentOperations(ref('01900000-0000-7000-8000-000000000002')),
      load: (state: ReturnType<typeof useEnvironmentOperations>) => state.load(),
      read: (state: ReturnType<typeof useEnvironmentOperations>) => state.operations,
      client: listEnvironmentOperations,
      code: 'OPERATION_LIST_RESPONSE_INVALID',
    },
    {
      create: () => useEnvironmentInstance(ref('01900000-0000-7000-8000-000000000003')),
      load: (state: ReturnType<typeof useEnvironmentInstance>) => state.load(),
      read: (state: ReturnType<typeof useEnvironmentInstance>) => state.instance,
      client: getEnvironment,
      code: 'ENVIRONMENT_RESPONSE_INVALID',
    },
  ])('fails closed with $code when the generated client returns no data', async ({ create, load, read, client, code }) => {
    vi.mocked(client).mockResolvedValue({} as never)
    const state = create() as never

    await load(state)

    const result = read(state)
    expect(result.kind).toBe('error')
    if (result.kind === 'error') {
      expect(result.diagnostic.code).toBe(code)
      expect(result.diagnostic.retryable).toBe(false)
    }
  })
})
