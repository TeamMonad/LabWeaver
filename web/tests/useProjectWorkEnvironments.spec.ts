import { effectScope, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { fetchProjectWorkEnvironments, useProjectWorkEnvironments } from '@/composables/useProjectWorkEnvironments'

const listEnvironments = vi.hoisted(() => vi.fn())

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, listEnvironments }
})

function page(items: unknown[], nextCursor: string | null = null) {
  return { data: { items, nextCursor } as never, error: undefined as never }
}

function environment(id: string) {
  return { id } as never
}

afterEach(() => {
  listEnvironments.mockReset()
})

describe('fetchProjectWorkEnvironments', () => {
  it('loads every page at the public limit', async () => {
    listEnvironments
      .mockResolvedValueOnce(page([environment('environment-1')], 'cursor-2'))
      .mockResolvedValueOnce(page([environment('environment-2')]))

    await expect(fetchProjectWorkEnvironments('project-1')).resolves.toEqual([
      environment('environment-1'),
      environment('environment-2'),
    ])
    expect(listEnvironments).toHaveBeenNthCalledWith(1, {
      query: { projectId: 'project-1', class: 'work', limit: 100 },
    })
    expect(listEnvironments).toHaveBeenNthCalledWith(2, {
      query: { projectId: 'project-1', class: 'work', limit: 100, cursor: 'cursor-2' },
    })
  })

  it('fails on a repeated cursor before returning partial data', async () => {
    listEnvironments
      .mockResolvedValueOnce(page([environment('environment-1')], 'cursor-2'))
      .mockResolvedValueOnce(page([environment('environment-2')], 'cursor-2'))

    await expect(fetchProjectWorkEnvironments('project-1')).rejects.toMatchObject({
      diagnosticCode: 'PROJECT_WORK_ENVIRONMENTS_CURSOR_REPEATED',
    })
    expect(listEnvironments).toHaveBeenCalledTimes(2)
  })
})

describe('useProjectWorkEnvironments', () => {
  it('does not let an older project response overwrite the selected project', async () => {
    let resolveFirst!: (value: unknown) => void
    let resolveSecond!: (value: unknown) => void
    const first = new Promise((resolve) => { resolveFirst = resolve })
    const second = new Promise((resolve) => { resolveSecond = resolve })
    listEnvironments.mockImplementation(({ query }: { query: { projectId: string } }) => (
      query.projectId === 'project-1' ? first : second
    ))

    const projectId = ref<string | null>('project-1')
    const scope = effectScope()
    let state!: ReturnType<typeof useProjectWorkEnvironments>
    scope.run(() => { state = useProjectWorkEnvironments(projectId) })
    await vi.waitFor(() => expect(listEnvironments).toHaveBeenCalledTimes(1))

    projectId.value = 'project-2'
    await vi.waitFor(() => expect(listEnvironments).toHaveBeenCalledTimes(2))
    resolveSecond(page([environment('environment-2')]))
    await vi.waitFor(() => expect(state.environments).toEqual({
      kind: 'success',
      data: [environment('environment-2')],
    }))

    resolveFirst(page([environment('environment-1')]))
    await Promise.resolve()
    expect(state.environments).toEqual({
      kind: 'success',
      data: [environment('environment-2')],
    })
    scope.stop()
  })
})
