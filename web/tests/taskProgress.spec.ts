import { describe, expect, it, vi } from 'vitest'
import { collectTaskProgress } from '@/utils/taskProgress'

const progress = (nextCursor: unknown, history = [1], revision = 1) => ({
  projectId: 'project', runId: 'run', revision, current: [{ attempt: revision }], history, nextCursor,
})

describe('collectTaskProgress', () => {
  it('reads every history page and keeps the most recent current attempt', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce({ data: progress('older', [3], 4) })
      .mockResolvedValueOnce({ data: progress(null, [2, 1], 5) })
    const response = await collectTaskProgress(fetch)
    expect(fetch.mock.calls).toEqual([[undefined], ['older']])
    expect(response.data).toMatchObject({ current: [{ attempt: 5 }], history: [3, 2, 1], nextCursor: null })
  })

  it.each([undefined, '', false, 0, [], {}])('fails closed on a bad later cursor %j', async (cursor) => {
    const fetch = vi.fn()
      .mockResolvedValueOnce({ data: progress('older') })
      .mockResolvedValueOnce({ data: progress(cursor) })
    await expect(collectTaskProgress(fetch)).rejects.toThrow('游标')
  })

  it('rejects loops rather than publishing a partial history', async () => {
    const fetch = vi.fn().mockResolvedValue({ data: progress('loop') })
    await expect(collectTaskProgress(fetch)).rejects.toThrow('重复')
    expect(fetch).toHaveBeenCalledTimes(2)
  })

  it('does not combine histories from different task scopes', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce({ data: progress('older') })
      .mockResolvedValueOnce({ data: { ...progress(null), runId: 'other' } })
    await expect(collectTaskProgress(fetch)).rejects.toThrow('其他项目或任务')
  })

  it('rejects the complete reading when the actor loses permission on the next page', async () => {
    const denied = { status: 403, diagnosticCode: 'LW_AUTH_FORBIDDEN' }
    const fetch = vi.fn()
      .mockResolvedValueOnce({ data: progress('older') })
      .mockResolvedValueOnce({ error: denied })
    await expect(collectTaskProgress(fetch)).rejects.toEqual(denied)
  })
})
