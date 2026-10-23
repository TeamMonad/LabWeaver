export type CursorPage<T> = {
  items: T[]
  nextCursor?: string | null
}

type CursorPageResponse<T> = {
  data: CursorPage<T>
  error?: undefined
} | {
  data?: undefined
  error: unknown
}

export async function collectCursorPages<T>(
  fetchPage: (cursor?: string) => Promise<CursorPageResponse<T>>,
): Promise<CursorPage<T>> {
  const items: T[] = []
  const seen = new Set<string>()
  let cursor: string | undefined
  let complete = false
  while (!complete) {
    const result = await fetchPage(cursor)
    if (!result.data) throw result.error
    items.push(...result.data.items)
    const nextCursor = result.data.nextCursor ?? undefined
    if (!nextCursor) {
      complete = true
      continue
    }
    if (seen.has(nextCursor)) throw new Error('Cursor pagination returned a repeated cursor')
    seen.add(nextCursor)
    cursor = nextCursor
  }
  return { items, nextCursor: null }
}
