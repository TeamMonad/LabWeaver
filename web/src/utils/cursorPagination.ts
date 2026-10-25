export type CursorPage<T> = {
  items: T[]
  nextCursor?: string | null
}

export type CursorPageResponse<T> = {
  data: CursorPage<T>
  error?: undefined
} | {
  data?: undefined
  error: unknown
}

export type CursorPageFetcher<T> = (cursor?: string) => Promise<CursorPageResponse<T>>

/** Only explicit null ends a list; a missing, empty or non-string cursor rejects the page. */
export async function readCursorPage<T>(
  fetchPage: CursorPageFetcher<T>,
  cursor?: string,
): Promise<{ items: T[]; nextCursor: string | null }> {
  const result = await fetchPage(cursor)
  if (!result.data) throw result.error
  if (!Array.isArray(result.data.items)) throw new Error('分页响应无效：缺少记录数组，无法确认列表完整性。')
  const nextCursor = result.data.nextCursor
  if (nextCursor !== null && (typeof nextCursor !== 'string' || nextCursor.length === 0)) {
    throw new Error('分页响应的续页游标无效，无法确认列表完整性。请刷新重试。')
  }
  return { items: result.data.items, nextCursor }
}

/**
 * Read up to `maxPages` pages from the start of a list (all pages when
 * omitted). The returned `nextCursor` continues after the last page read and
 * is null once the server reports the list complete. A malformed page or a
 * repeated cursor throws instead of looping or returning a partial list.
 */
async function readCursorPages<T>(
  fetchPage: CursorPageFetcher<T>,
  maxPages?: number,
): Promise<{ items: T[]; nextCursor: string | null; pages: number }> {
  const items: T[] = []
  const seen = new Set<string>()
  let cursor: string | undefined
  let pages = 0
  for (;;) {
    const page = await readCursorPage(fetchPage, cursor)
    pages += 1
    items.push(...page.items)
    if (page.nextCursor === null) return { items, nextCursor: null, pages }
    if (seen.has(page.nextCursor)) throw new Error('分页响应重复了续页游标，无法确认列表完整性。请刷新重试。')
    seen.add(page.nextCursor)
    if (maxPages !== undefined && pages >= maxPages) return { items, nextCursor: page.nextCursor, pages }
    cursor = page.nextCursor
  }
}

/** Follow `nextCursor` until the server says the list is complete. */
export async function collectCursorPages<T>(fetchPage: CursorPageFetcher<T>): Promise<CursorPage<T>> {
  const { items } = await readCursorPages(fetchPage)
  return { items, nextCursor: null }
}

/**
 * Read at most `pageCount` pages from the start of a list, keeping the cursor
 * that continues after them. A "load more" view re-reads the window it has
 * already shown this way, so a refresh neither drops nor silently extends it.
 */
export async function readCursorWindow<T>(
  fetchPage: CursorPageFetcher<T>,
  pageCount: number,
): Promise<{ items: T[]; nextCursor: string | null; pages: number }> {
  return readCursorPages(fetchPage, Math.max(1, pageCount))
}
