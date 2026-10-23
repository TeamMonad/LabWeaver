export type CursorPage<T> = {
  items: T[]
  nextCursor?: string | null
}

export async function collectCursorPages<T>(
  fetchPage: (cursor?: string) => Promise<any>,
): Promise<CursorPage<T>> {
  const items: T[] = []
  const seen = new Set<string>()
  let cursor: string | undefined
  while (true) {
    const result = await fetchPage(cursor)
    if (result.error) throw result.error
    items.push(...result.data.items)
    const nextCursor = result.data.nextCursor ?? undefined
    if (!nextCursor) return { items, nextCursor: null }
    if (seen.has(nextCursor)) throw new Error('Cursor pagination returned a repeated cursor')
    seen.add(nextCursor)
    cursor = nextCursor
  }
}
