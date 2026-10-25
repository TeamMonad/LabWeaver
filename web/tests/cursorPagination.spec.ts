import { describe, expect, it, vi } from 'vitest'
import { collectCursorPages, readCursorPage, type CursorPageResponse } from '@/utils/cursorPagination'

// Model an untrusted JSON response; the generated SDK's static type cannot validate the wire.
function response(items: number[], nextCursor: unknown): CursorPageResponse<number> {
  return { data: { items, nextCursor } } as unknown as CursorPageResponse<number>
}

describe('cursor pagination response validation', () => {
  it.each([false, 0, 1, true, undefined, {}, [], ''])('rejects invalid continuation %j with a Chinese diagnostic', async (nextCursor) => {
    await expect(readCursorPage(async () => response([1], nextCursor)))
      .rejects.toThrow('分页响应的续页游标无效，无法确认列表完整性。')
  })

  it.each([false, 0, 1, true, undefined, {}, [], ''])('never returns accumulated items when a later page has continuation %j', async (nextCursor) => {
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(response([1], 'page-2'))
      .mockResolvedValueOnce(response([2], nextCursor))

    await expect(collectCursorPages(fetchPage)).rejects.toThrow('分页响应的续页游标无效')
    expect(fetchPage.mock.calls).toEqual([[undefined], ['page-2']])
  })

  it('accepts only explicit null as a normal last page', async () => {
    const fetchPage = vi.fn().mockResolvedValue(response([1], null))

    await expect(collectCursorPages(fetchPage)).resolves.toEqual({ items: [1], nextCursor: null })
    expect(fetchPage).toHaveBeenCalledTimes(1)
  })

  it('follows non-empty strings and returns all items after the null last page', async () => {
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(response([1], 'page-2'))
      .mockResolvedValueOnce(response([2], null))

    await expect(collectCursorPages(fetchPage)).resolves.toEqual({ items: [1, 2], nextCursor: null })
    expect(fetchPage.mock.calls).toEqual([[undefined], ['page-2']])
  })

  it('rejects repeated cursors instead of returning an incomplete collection', async () => {
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(response([1], 'page-2'))
      .mockResolvedValueOnce(response([2], 'page-2'))

    await expect(collectCursorPages(fetchPage)).rejects.toThrow('分页响应重复了续页游标')
    expect(fetchPage).toHaveBeenCalledTimes(2)
  })
})
