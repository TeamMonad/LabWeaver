import { computed, shallowRef, toRaw, watch } from 'vue'
import { useRoute, useRouter, type LocationQueryRaw } from 'vue-router'
import { getProject, type ProjectSchema } from '@/generated/contracts'
import { useProjects } from '@/composables/useProjects'
import { diagnosticFromError, type AsyncState, type DiagnosticViewModel } from '@/types/async'

/**
 * The browser-wide project context.
 *
 * `?projectId=` in the address is authoritative on project-scoped pages. The
 * selector, page title, requests and rendered results all follow it:
 * - a scoped page without the query gets the current selection written into
 *   the address, so reloads and copied links name the same project;
 * - a linked project the account cannot list fails closed with a recovery
 *   entry instead of rendering the previous project's data.
 *
 * Only the server project list and `getProject` decide accessibility; the
 * browser never synthesizes a project.
 */
export type ProjectContextState =
  | { kind: 'unscoped' }
  | { kind: 'loading' }
  | { kind: 'ready'; project: ProjectSchema }
  | { kind: 'none' }
  | { kind: 'list-error'; diagnostic: DiagnosticViewModel }
  | {
      kind: 'unavailable'
      projectId: string
      reason: 'checking' | 'not-accessible' | 'not-listed' | 'archived-or-hidden' | 'error'
      diagnostic: DiagnosticViewModel | null
      fallback: ProjectSchema | null
    }

/** Query keys that name objects inside one project; they are dropped when the project changes. */
export const PROJECT_OWNED_QUERY_KEYS = ['environmentId', 'releaseId', 'runId', 'packageId', 'approvalId'] as const

type ProjectList = AsyncState<ProjectSchema[]>
type ProbeResult = 'checking' | { project: ProjectSchema } | { diagnostic: DiagnosticViewModel }

/**
 * Why a linked project is missing from the account's list. A result is only
 * valid for the list it was taken against: any list reload (identity,
 * membership or permission change) discards it and asks again.
 */
const probe = shallowRef<{ projectId: string; list: ProjectList; result: ProbeResult } | null>(null)
let probeGeneration = 0
/** The linked project for which the list was already reloaded once before probing. */
let listRefreshedFor: string | null = null

async function probeProject(projectId: string, list: ProjectList) {
  if (probe.value?.projectId === projectId && probe.value.list === list) return
  const generation = ++probeGeneration
  probe.value = { projectId, list, result: 'checking' }
  const result = await getProject({ path: { projectId } })
  if (generation !== probeGeneration) return
  probe.value = result.error
    ? { projectId, list, result: { diagnostic: diagnosticFromError(result.error, 'PROJECT_CONTEXT_UNAVAILABLE', '无法读取链接中的项目。', false) } }
    : { projectId, list, result: { project: result.data } }
}

function routeProjectIdOf(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value.trim() : null
}

export function useProjectContext() {
  const route = useRoute()
  const projects = useProjects()

  const scoped = computed(() => Boolean(route.meta.projectScoped))
  const routeProjectId = computed(() => routeProjectIdOf(route.query.projectId))

  const fallbackProject = computed<ProjectSchema | null>(() => {
    const settled = projects.settledProjects
    if (settled.kind !== 'success') return null
    const requested = routeProjectId.value
    const candidates = settled.data.filter((project) => project.id !== requested)
    return candidates.find((project) => project.id === projects.selectedProjectId)
      ?? candidates.find((project) => project.state === 'active')
      ?? candidates[0]
      ?? null
  })

  const state = computed<ProjectContextState>(() => {
    if (!scoped.value) return { kind: 'unscoped' }
    const settled = projects.settledProjects
    const requested = routeProjectId.value
    if (settled.kind === 'idle' || settled.kind === 'loading') return { kind: 'loading' }
    if (settled.kind === 'error') {
      return { kind: 'list-error', diagnostic: settled.diagnostic }
    }
    if (settled.kind !== 'success' && settled.kind !== 'empty') {
      return { kind: 'list-error', diagnostic: settled.diagnostic }
    }
    const items = settled.kind === 'success' ? settled.data : []
    if (!requested) {
      const selected = items.find((project) => project.id === projects.selectedProjectId) ?? null
      return selected ? { kind: 'ready', project: selected } : { kind: 'none' }
    }
    const listed = items.find((project) => project.id === requested)
    if (listed) return { kind: 'ready', project: listed }
    // Pages after the loaded prefix are still unread: a failed page is a list
    // error whose retry continues locating, and pages still being followed are
    // loading; neither is a verdict on the project.
    if (projects.locating === requested) {
      return projects.loadMoreError ? { kind: 'list-error', diagnostic: projects.loadMoreError } : { kind: 'loading' }
    }
    const current = probe.value
    const checked = current?.projectId === requested && current.list === toRaw(settled) ? current.result : 'checking'
    if (checked === 'checking') {
      return { kind: 'unavailable', projectId: requested, reason: 'checking', diagnostic: null, fallback: fallbackProject.value }
    }
    if ('project' in checked) {
      // Readable but still absent from a freshly reloaded list. Pages, the
      // selector and requests all follow the list, so the context fails
      // closed instead of pairing this project with the previous selection.
      return {
        kind: 'unavailable',
        projectId: requested,
        reason: checked.project.state === 'active' ? 'not-listed' : 'archived-or-hidden',
        diagnostic: null,
        fallback: fallbackProject.value,
      }
    }
    const status = checked.diagnostic.status
    return {
      kind: 'unavailable',
      projectId: requested,
      // Access may answer 403 before it knows whether the project exists, so
      // 401/403/404 are one user-facing reason: missing or not accessible.
      reason: status === 404 || status === 403 || status === 401 ? 'not-accessible' : 'error',
      diagnostic: checked.diagnostic,
      fallback: fallbackProject.value,
    }
  })

  const project = computed(() => state.value.kind === 'ready' ? state.value.project : null)

  return { state, project, routeProjectId, scoped }
}

/**
 * Keep address, selector and title on one project. Installed once by the
 * AppShell; views read the resolved project from `useProjects()` or the route.
 */
export function useProjectContextSync() {
  const route = useRoute()
  const router = useRouter()
  const projects = useProjects()
  const context = useProjectContext()

  watch(
    [() => context.scoped.value, () => context.routeProjectId.value, () => projects.settledProjects],
    ([scoped, requested, settled]) => {
      if (!scoped || (settled.kind !== 'success' && settled.kind !== 'empty')) return
      const items = settled.kind === 'success' ? settled.data : []
      if (requested) {
        if (items.some((project) => project.id === requested)) {
          listRefreshedFor = null
          if (projects.selectedProjectId !== requested) projects.select(requested)
        } else if (projects.locating === requested) {
          // Pages are still being followed for it (or a page failed and waits
          // for a retry); only a listed target or the end of the list settles it.
        } else if (listRefreshedFor !== requested) {
          // The project may sit on a later page or the list may predate a
          // membership change; reload and follow the pages once before asking why.
          listRefreshedFor = requested
          void projects.locate(requested)
        } else {
          void probeProject(requested, toRaw(settled))
        }
        return
      }
      listRefreshedFor = null
      const selected = projects.selectedProjectId
      if (!selected || !items.some((project) => project.id === selected)) return
      const query: LocationQueryRaw = { ...route.query, projectId: selected }
      void router.replace({ path: route.path, query, hash: route.hash })
    },
    { immediate: true },
  )

  watch(
    [() => route.meta.title, () => context.project.value?.name, () => context.state.value.kind],
    ([title, projectName, kind]) => {
      if (typeof document === 'undefined') return
      const page = typeof title === 'string' && title ? title : 'LabWeaver'
      if (kind === 'unavailable' || kind === 'list-error') document.title = `项目不可用 · ${page} · LabWeaver`
      else if (projectName) document.title = `${page} · ${projectName} · LabWeaver`
      else document.title = page === 'LabWeaver' ? page : `${page} · LabWeaver`
    },
    { immediate: true },
  )

  // After a failed locate page the retry continues from that page; otherwise reload the list.
  return { ...context, reloadProjects: () => projects.locating && projects.loadMoreError ? projects.loadMore() : projects.load() }
}

/** Address of the same page in another project, without the previous project's object ids. */
export function projectSwitchLocation(
  path: string,
  query: Record<string, unknown>,
  projectId: string,
): { path: string; query: LocationQueryRaw } {
  const next: LocationQueryRaw = { ...(query as LocationQueryRaw), projectId }
  for (const key of PROJECT_OWNED_QUERY_KEYS) next[key] = undefined
  return { path, query: next }
}
