import { collectCursorPages } from './cursorPagination'

type TaskProgress = {
  projectId: string
  runId: string
  revision: number
  history: unknown[]
  nextCursor?: string | null
}

/** Collect complete attempt history; malformed or cross-task pages never publish a partial reading. */
export async function collectTaskProgress<T extends TaskProgress>(
  fetchPage: (cursor?: string) => Promise<{ data?: T; error?: unknown }>,
): Promise<{ data: T; error?: undefined }> {
  let latest: T | undefined
  const history = await collectCursorPages<T['history'][number]>(async (cursor) => {
    const response = await fetchPage(cursor)
    if (!response.data) throw response.error
    const progress = response.data
    if (latest && (progress.projectId !== latest.projectId || progress.runId !== latest.runId)) {
      throw new Error('任务续页返回了其他项目或任务的数据，已停止显示。请刷新重试。')
    }
    if (!latest || progress.revision >= latest.revision) latest = progress
    return { data: { items: progress.history, nextCursor: progress.nextCursor } }
  })
  if (!latest) throw new Error('任务进度响应为空，无法确认历史完整性。')
  return { data: { ...latest, history: history.items, nextCursor: null } }
}
