import { reactive, ref, watch, type Ref } from 'vue'
import { listEnvironmentTemplateReleases } from '@/generated/contracts'
import type { EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'

interface EnvironmentTemplateReleaseCollection {
  items: EnvironmentTemplateReleaseViewSchema[]
  error?: unknown
}

/**
 * Read every release page for one project.  Project membership and the
 * private/shared publication rules are enforced by Auth; the browser only
 * follows the server cursor and never reconstructs that visibility policy.
 */
export async function fetchEnvironmentTemplateReleases(
  projectId: string,
  courseId?: string,
): Promise<EnvironmentTemplateReleaseCollection> {
  const items: EnvironmentTemplateReleaseViewSchema[] = []
  const seenCursors = new Set<string>()
  let cursor: string | undefined

  for (;;) {
    const result = await listEnvironmentTemplateReleases({
      path: { projectId },
      query: {
        limit: 100,
        ...(courseId ? { courseId } : {}),
        ...(cursor ? { cursor } : {}),
      },
    })
    if (result.error) return { items, error: result.error }

    items.push(...result.data.items)
    const nextCursor = result.data.nextCursor ?? undefined
    if (!nextCursor) return { items }
    if (seenCursors.has(nextCursor)) {
      return { items, error: new Error('Environment template release API returned a repeated cursor') }
    }
    seenCursors.add(nextCursor)
    cursor = nextCursor
  }
}

export function useEnvironmentTemplateReleases(
  projectId: Ref<string | undefined>,
  courseId: Ref<string | undefined> = ref(undefined),
) {
  const releases = ref<AsyncState<EnvironmentTemplateReleaseViewSchema[]>>({ kind: 'idle' })

  async function load() {
    const id = projectId.value
    if (!id) {
      releases.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic(
          'PROJECT_CONTEXT_MISSING',
          '项目上下文未绑定，无法加载环境模板版本。请先选择有权访问的项目。',
          false,
        ),
      }
      return
    }

    releases.value = { kind: 'loading', message: '加载已发布版本…' }
    const result = await fetchEnvironmentTemplateReleases(id, courseId.value)
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      releases.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'RELEASE_LIST_FAILED',
          problem?.detail ?? '加载环境模板版本失败',
          problem?.retryable ?? true,
        ),
      }
      return
    }
    const items = result.items ?? []
    releases.value = items.length > 0 ? { kind: 'success', data: items } : { kind: 'empty' }
  }

  watch([projectId, courseId], load, { immediate: true })

  return reactive({ releases, load })
}
