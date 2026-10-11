import { onScopeDispose, reactive, ref, watch, type Ref } from 'vue'
import { listEnvironments } from '@/generated/contracts'
import type { EnvironmentSummary } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'

export class ProjectWorkEnvironmentPaginationError extends Error {
  constructor(
    readonly diagnosticCode:
      | 'PROJECT_WORK_ENVIRONMENTS_INVALID'
      | 'PROJECT_WORK_ENVIRONMENTS_CURSOR_REPEATED'
      | 'PROJECT_ENVIRONMENTS_INVALID'
      | 'PROJECT_ENVIRONMENTS_CURSOR_REPEATED',
    message: string,
  ) {
    super(message)
    this.name = 'ProjectWorkEnvironmentPaginationError'
  }
}

export type ProjectEnvironmentListQuery = {
  projectId: string
  courseId?: string
  runtimeKind?: 'container' | 'virtual_machine'
  class?: 'experiment' | 'work'
  desiredState?: 'running' | 'stopped' | 'deleted'
  observedState?: 'requested' | 'validating' | 'building' | 'provisioning' | 'ready' | 'stopping' | 'stopped' | 'updating' | 'expiring' | 'deleting' | 'deleted' | 'failed'
  releaseId?: string
  limit?: number
}

/** Reads every project environment page using the server's bounded cursor contract. */
export async function fetchProjectEnvironments(
  query: ProjectEnvironmentListQuery,
  options: { diagnosticPrefix?: 'PROJECT_ENVIRONMENTS' | 'PROJECT_WORK_ENVIRONMENTS' } = {},
): Promise<EnvironmentSummary[]> {
  const items: EnvironmentSummary[] = []
  const seenCursors = new Set<string>()
  let cursor: string | undefined
  const prefix = options.diagnosticPrefix ?? 'PROJECT_ENVIRONMENTS'

  for (;;) {
    const result = await listEnvironments({
      query: {
        ...query,
        ...(cursor ? { cursor } : {}),
      },
    })
    if (result.error) throw result.error

    const page = result.data
    if (!page || !Array.isArray(page.items)) {
      throw new ProjectWorkEnvironmentPaginationError(
        prefix === 'PROJECT_WORK_ENVIRONMENTS' ? 'PROJECT_WORK_ENVIRONMENTS_INVALID' : 'PROJECT_ENVIRONMENTS_INVALID',
        prefix === 'PROJECT_WORK_ENVIRONMENTS'
          ? '环境服务返回了无法识别的 Work 列表。'
          : '环境服务返回了无法识别的项目环境列表。',
      )
    }
    items.push(...page.items)

    const nextCursor = page.nextCursor ?? undefined
    if (!nextCursor) return items
    if (seenCursors.has(nextCursor)) {
      throw new ProjectWorkEnvironmentPaginationError(
        prefix === 'PROJECT_WORK_ENVIRONMENTS' ? 'PROJECT_WORK_ENVIRONMENTS_CURSOR_REPEATED' : 'PROJECT_ENVIRONMENTS_CURSOR_REPEATED',
        prefix === 'PROJECT_WORK_ENVIRONMENTS'
          ? 'Work 环境列表分页游标重复，无法继续加载。'
          : '项目环境列表分页游标重复，无法继续加载。',
      )
    }
    seenCursors.add(nextCursor)
    cursor = nextCursor
  }
}

/** Reads every Work environment page using the server's bounded cursor contract. */
export async function fetchProjectWorkEnvironments(projectId: string): Promise<EnvironmentSummary[]> {
  return fetchProjectEnvironments(
    { projectId, class: 'work', limit: 100 },
    { diagnosticPrefix: 'PROJECT_WORK_ENVIRONMENTS' },
  )
}

function errorDiagnostic(error: unknown): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(
    problem?.diagnosticCode ?? 'PROJECT_WORK_ENVIRONMENTS_LOAD_FAILED',
    problem?.detail ?? '加载 Work 环境失败',
    problem?.retryable ?? true,
  )
}

/**
 * Loads the complete Work inventory for one Project. The Environment API owns
 * visibility and lifecycle state; the browser only supplies the project scope
 * and follows the server cursor.
 */
export function useProjectWorkEnvironments(projectId: Ref<string | null | undefined>) {
  const environments = ref<AsyncState<EnvironmentSummary[]>>({ kind: 'idle' })
  let loadGeneration = 0

  async function load() {
    const id = projectId.value
    const generation = ++loadGeneration
    if (!id) {
      environments.value = { kind: 'idle' }
      return
    }

    environments.value = { kind: 'loading', message: '加载 Work 环境…' }
    let items: EnvironmentSummary[]
    try {
      items = await fetchProjectWorkEnvironments(id)
    } catch (error) {
      if (generation !== loadGeneration) return
      const paginationError = error instanceof ProjectWorkEnvironmentPaginationError
      environments.value = {
        kind: 'error',
        diagnostic: paginationError
          ? makeDiagnostic(error.diagnosticCode, error.message, false)
          : errorDiagnostic(error),
      }
      return
    }
    if (generation !== loadGeneration) return

    environments.value = items.length > 0 ? { kind: 'success', data: items } : { kind: 'empty' }
  }

  watch(projectId, () => void load(), { immediate: true })
  onScopeDispose(() => { loadGeneration += 1 })

  return reactive({ environments, load })
}
