import { computed, onMounted, onScopeDispose, reactive, ref, watch, type Ref } from 'vue'
import {
  addProjectMembership,
  archiveProject,
  createProject,
  getProject,
  listOrganizationUsers,
  listProjectMemberships,
  listProjects,
  removeProjectMembership,
  updateProject,
} from '@/generated/contracts'
import type {
  AddProjectMembershipRequestSchema,
  OrganizationUserPage,
  ProjectMembershipSchema,
  ProjectSchema,
  UpdateProjectRequestSchema,
} from '@/generated/contracts'
import { sessionEpoch } from '@/api/session'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'
import { readCursorPage } from '@/utils/cursorPagination'

function diagnostic(error: unknown, fallbackCode: string, fallbackMessage: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackMessage, problem?.retryable ?? true, problem)
}

function directoryDiagnostic(error: unknown): DiagnosticViewModel {
  const result = diagnostic(error, 'DIRECTORY_USERS_LIST_FAILED', '查找组织账号失败')
  return result.code === 'LW_ACCESS_DIRECTORY_UNAVAILABLE'
    ? { ...result, message: '平台账号目录暂时不可用，请稍后重试。已创建的项目和材料会保留。' }
    : result
}

export interface ProjectMutationResult {
  kind: 'success' | 'error'
  diagnostic: DiagnosticViewModel
}

function persistedProjectId(): string | null {
  if (typeof localStorage === 'undefined') return null
  const value = localStorage.getItem('labweaver_project_id')
  const projectId = value?.trim()
  return projectId || null
}

/** Server default page size of the Control cursor lists. */
export const CONTROL_PAGE_LIMIT = 50

function pageQuery(cursor: string | null): { limit: number; cursor?: string } {
  return { limit: CONTROL_PAGE_LIMIT, ...(cursor ? { cursor } : {}) }
}

/** Keyset pages never overlap; the id guard only keeps a moved row from rendering twice. */
function appendPage<T>(loaded: readonly T[], page: readonly T[], key: (item: T) => string): T[] {
  const seen = new Set(loaded.map(key))
  return [...loaded, ...page.filter((item) => !seen.has(key(item)))]
}

// The catalog is always a prefix of the server's cursor order: the loaded
// pages, plus the cursor of the next one while the list continues.
const projects = ref<AsyncState<ProjectSchema[]>>({ kind: 'idle' })
// Last finished list result. Consumers that must not flicker while a sibling
// component reloads the shared catalog (the AppShell project gate) read this.
const settledProjects = ref<AsyncState<ProjectSchema[]>>({ kind: 'idle' })
const projectsNextCursor = ref<string | null>(null)
const loadingMoreProjects = ref(false)
const loadMoreProjectsError = ref<DiagnosticViewModel | null>(null)
// Restore the browser's existing project context before the first async list
// response so project-scoped links remain scoped during a page reload.
const selectedProjectId = ref<string | null>(persistedProjectId())
const acting = ref<string | null>(null)
const outcome = ref<ProjectMutationResult | null>(null)
let loadGeneration = 0
let projectCursors = new Set<string>()
// A linked project the catalog is still following pages for. It stays wanted
// across superseding reloads and failed pages, and is only dropped once it is
// listed, the list ends, a listed project is selected or the session changes.
const locatingProjectId = ref<string | null>(null)

const selectedProject = computed<ProjectSchema | null>(() => {
  if (projects.value.kind !== 'success' || !selectedProjectId.value) return null
  return projects.value.data.find((project) => project.id === selectedProjectId.value) ?? null
})

function preserveSelection(items: ProjectSchema[]) {
  if (items.length === 0) {
    selectedProjectId.value = null
    return
  }
  const saved = persistedProjectId()
  if (!selectedProjectId.value || !items.some((project) => project.id === selectedProjectId.value)) {
    selectedProjectId.value = saved && items.some((project) => project.id === saved) ? saved : items[0].id
  }
}

/** Pages read by one load: the catalog prefix, the first unread cursor and why reading stopped early. */
interface FollowedPages {
  items: ProjectSchema[]
  cursor: string | null
  failure: DiagnosticViewModel | null
}

/**
 * Read the page at `cursor` (the first page when null), then further pages
 * while the current or located project is still unlisted. A failed page keeps
 * what was read and its own cursor so a retry continues from there.
 * Returns null when a newer reload superseded this one.
 */
async function followPages(generation: number, loaded: ProjectSchema[], cursor: string | null): Promise<FollowedPages | null> {
  const targets = [selectedProjectId.value, locatingProjectId.value].filter((id): id is string => Boolean(id))
  let items = loaded
  let next = cursor
  const seenCursors = projectCursors
  do {
    try {
      const page = await readCursorPage((cursor) => listProjects({ query: pageQuery(cursor ?? null) }), next ?? undefined, seenCursors)
      if (generation !== loadGeneration) return null
      items = appendPage(items, page.items, (project) => project.id)
      next = page.nextCursor
    } catch (error) {
      if (generation !== loadGeneration) return null
      const failure = next
        ? diagnostic(error, 'PROJECT_LIST_MORE_FAILED', '加载更多项目失败')
        : diagnostic(error, 'PROJECT_LIST_FAILED', '加载项目失败')
      return { items, cursor: next, failure }
    }
  } while (next !== null && targets.some((id) => !items.some((project) => project.id === id)))
  return { items, cursor: next, failure: null }
}

function settlePages({ items, cursor, failure }: FollowedPages) {
  loadingMoreProjects.value = false
  if (failure && items.length === 0) {
    // The first page failed. The locate target stays so the retry looks for it again.
    projectsNextCursor.value = null
    loadMoreProjectsError.value = null
    projects.value = { kind: 'error', diagnostic: failure }
  } else {
    projectsNextCursor.value = cursor
    loadMoreProjectsError.value = failure
    // A partial catalog says nothing about the unread pages: keep the current
    // selection and the locate target until the pages are read.
    if (!failure) {
      locatingProjectId.value = null
      preserveSelection(items)
    }
    projects.value = items.length > 0 ? { kind: 'success', data: items } : { kind: 'empty' }
  }
  settledProjects.value = projects.value
}

/**
 * Reload the catalog from its first page. The current project and `wanted`
 * may sit on a later page; pages are followed until they are listed or the
 * list ends, so the context is never replaced just because it was not on page one.
 */
async function loadProjects(wanted: string | null = null) {
  const generation = ++loadGeneration
  projectCursors = new Set<string>()
  if (wanted) locatingProjectId.value = wanted
  projects.value = { kind: 'loading', message: '加载项目…' }
  loadingMoreProjects.value = false
  loadMoreProjectsError.value = null
  const followed = await followPages(generation, [], null)
  if (followed) settlePages(followed)
}

/**
 * Append the next server page to the loaded catalog. After a failed locate
 * this is the retry: it continues from the failed page until the located
 * project is listed or the list ends.
 */
async function loadMoreProjects() {
  const cursor = projectsNextCursor.value
  const loaded = projects.value
  if (!cursor || loadingMoreProjects.value || loaded.kind !== 'success') return
  const generation = loadGeneration
  loadingMoreProjects.value = true
  loadMoreProjectsError.value = null
  const followed = await followPages(generation, loaded.data, cursor)
  if (followed) settlePages(followed)
}

// The previous actor's list must not decide the new actor's project context:
// the AppShell waits for this actor's list before it renders a scoped page.
watch(sessionEpoch, () => {
  settledProjects.value = { kind: 'idle' }
  locatingProjectId.value = null
  void loadProjects()
})

/**
 * Project is the browser's durable context for Work, Agent and Resource calls.
 * The composable keeps only server projections; it never synthesizes a project
 * when the API is unavailable.
 */
export function useProjects() {
  // Template handlers pass DOM events; `load` never names a new locate target.
  async function load() {
    await loadProjects()
  }

  function select(projectId: string) {
    selectedProjectId.value = projectId
    // A listed project is now the context; an earlier link no longer needs locating.
    locatingProjectId.value = null
    if (typeof localStorage !== 'undefined') localStorage.setItem('labweaver_project_id', projectId)
    outcome.value = null
  }

  async function create(name: string, description?: string, courseId?: string): Promise<ProjectSchema | null> {
    if (acting.value) return null
    const trimmedName = name.trim()
    if (!trimmedName) {
      outcome.value = { kind: 'error', diagnostic: makeDiagnostic('PROJECT_NAME_REQUIRED', '项目名称不能为空。', false) }
      return null
    }
    acting.value = 'create'
    outcome.value = null
    try {
      const result = await createProject({
        headers: { 'Idempotency-Key': idempotencyKey() },
        body: {
          name: trimmedName,
          ...(description?.trim() ? { description: description.trim() } : {}),
          ...(courseId?.trim() ? { courseId: courseId.trim() } : {}),
        },
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: diagnostic(result.error, 'PROJECT_CREATE_FAILED', '创建项目失败') }
        return null
      }
      outcome.value = { kind: 'success', diagnostic: makeDiagnostic('PROJECT_CREATED', `项目 ${result.data.name} 已创建。`, false) }
      selectedProjectId.value = result.data.id
      if (typeof localStorage !== 'undefined') localStorage.setItem('labweaver_project_id', result.data.id)
      await load()
      selectedProjectId.value = result.data.id
      return result.data
    } finally {
      acting.value = null
    }
  }

  async function update(project: ProjectSchema, input: Omit<UpdateProjectRequestSchema, 'expectedRevision'>): Promise<boolean> {
    if (acting.value) return false
    acting.value = `update:${project.id}`
    outcome.value = null
    try {
      const result = await updateProject({
        path: { projectId: project.id },
        headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(project.revision) },
        body: { ...input, expectedRevision: project.revision },
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: diagnostic(result.error, 'PROJECT_UPDATE_FAILED', '更新项目失败') }
        return false
      }
      outcome.value = { kind: 'success', diagnostic: makeDiagnostic('PROJECT_UPDATED', '项目已更新。', false) }
      await load()
      selectedProjectId.value = result.data.id
      return true
    } finally {
      acting.value = null
    }
  }

  async function archive(project: ProjectSchema): Promise<boolean> {
    if (acting.value) return false
    acting.value = `archive:${project.id}`
    outcome.value = null
    try {
      const result = await archiveProject({
        path: { projectId: project.id },
        headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(project.revision) },
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: diagnostic(result.error, 'PROJECT_ARCHIVE_FAILED', '归档项目失败') }
        return false
      }
      outcome.value = { kind: 'success', diagnostic: makeDiagnostic('PROJECT_ARCHIVED', '项目已归档。', false) }
      await load()
      return true
    } finally {
      acting.value = null
    }
  }

  onMounted(() => {
    void loadProjects()
  })

  return reactive({
    projects,
    settledProjects,
    hasMore: computed(() => projectsNextCursor.value !== null),
    loadingMore: loadingMoreProjects,
    loadMoreError: loadMoreProjectsError,
    /** Linked project whose pages are still being followed (in flight or stopped by a failed page). */
    locating: computed(() => locatingProjectId.value),
    selectedProjectId,
    selectedProject,
    acting,
    outcome,
    load,
    /** Reload from the first page and follow the cursor until the project is listed or the list ends. */
    locate: loadProjects,
    loadMore: loadMoreProjects,
    select,
    create,
    update,
    archive,
  })
}

/** Load and mutate project-local Access memberships, one cursor page at a time. */
export function useProjectMemberships(projectId: Ref<string | null | undefined>) {
  const memberships = ref<AsyncState<ProjectMembershipSchema[]>>({ kind: 'idle' })
  const nextCursor = ref<string | null>(null)
  const loadingMore = ref(false)
  const loadMoreError = ref<DiagnosticViewModel | null>(null)
  const acting = ref<string | null>(null)
  const outcome = ref<ProjectMutationResult | null>(null)
  let loadGeneration = 0
  let seenCursors = new Set<string>()

  async function load() {
    const id = projectId.value
    const generation = ++loadGeneration
    seenCursors = new Set<string>()
    nextCursor.value = null
    loadingMore.value = false
    loadMoreError.value = null
    if (!id) {
      memberships.value = { kind: 'idle' }
      return
    }
    memberships.value = { kind: 'loading', message: '加载项目成员…' }
    try {
      const page = await readCursorPage(() => listProjectMemberships({ path: { projectId: id }, query: pageQuery(null) }), undefined, seenCursors)
      if (generation !== loadGeneration || projectId.value !== id) return
      nextCursor.value = page.nextCursor
      memberships.value = page.items.length > 0 ? { kind: 'success', data: page.items } : { kind: 'empty' }
    } catch (error) {
      if (generation !== loadGeneration || projectId.value !== id) return
      memberships.value = { kind: 'error', diagnostic: diagnostic(error, 'PROJECT_MEMBERS_LIST_FAILED', '加载项目成员失败') }
    }
  }

  async function loadMore() {
    const id = projectId.value
    const cursor = nextCursor.value
    const loaded = memberships.value
    if (!id || !cursor || loadingMore.value || loaded.kind !== 'success') return
    const generation = loadGeneration
    loadingMore.value = true
    loadMoreError.value = null
    try {
      const page = await readCursorPage(() => listProjectMemberships({ path: { projectId: id }, query: pageQuery(cursor) }), cursor, seenCursors)
      if (generation !== loadGeneration || projectId.value !== id) return
      loadingMore.value = false
      nextCursor.value = page.nextCursor
      memberships.value = { kind: 'success', data: appendPage(loaded.data, page.items, (membership) => membership.actorId) }
    } catch (error) {
      if (generation !== loadGeneration || projectId.value !== id) return
      loadingMore.value = false
      loadMoreError.value = diagnostic(error, 'PROJECT_MEMBERS_LIST_MORE_FAILED', '加载更多项目成员失败')
    }
  }

  watch(projectId, (id, previousId) => {
    if (id !== previousId) outcome.value = null
    void load()
  }, { immediate: true })
  onScopeDispose(() => { loadGeneration += 1 })

  async function add(input: AddProjectMembershipRequestSchema): Promise<boolean> {
    const id = projectId.value
    if (!id || acting.value) return false
    acting.value = 'add'
    outcome.value = null
    try {
      const result = await addProjectMembership({
        path: { projectId: id },
        headers: { 'Idempotency-Key': idempotencyKey() },
        body: input,
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: diagnostic(result.error, 'PROJECT_MEMBER_ADD_FAILED', '添加项目成员失败') }
        return false
      }
      outcome.value = { kind: 'success', diagnostic: makeDiagnostic('PROJECT_MEMBER_ADDED', '项目成员已添加。', false) }
      await load()
      return true
    } finally {
      acting.value = null
    }
  }

  async function remove(actorId: string, membership: ProjectMembershipSchema): Promise<boolean> {
    const id = projectId.value
    if (!id || acting.value) return false
    acting.value = `remove:${actorId}`
    outcome.value = null
    try {
      const result = await removeProjectMembership({
        path: { projectId: id, actorId },
        headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(membership.revision) },
        body: { expectedRevision: membership.revision, reason: 'project member removed by owner' },
      })
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: diagnostic(result.error, 'PROJECT_MEMBER_REMOVE_FAILED', '移除项目成员失败') }
        return false
      }
      outcome.value = { kind: 'success', diagnostic: makeDiagnostic('PROJECT_MEMBER_REMOVED', '项目成员已移除。', false) }
      await load()
      return true
    } finally {
      acting.value = null
    }
  }

  return reactive({
    memberships,
    hasMore: computed(() => nextCursor.value !== null),
    loadingMore,
    loadMoreError,
    acting,
    outcome,
    load,
    loadMore,
    add,
    remove,
  })
}

const ORGANIZATION_DIRECTORY_PAGE_SIZE = 25

/**
 * Search the issuer-backed organization directory for membership selection.
 * The page is server-authoritative; local state only tracks the latest query
 * and ignores responses for an older query or page.
 */
export function useOrganizationDirectoryUsers() {
  const users = ref<AsyncState<OrganizationUserPage>>({ kind: 'idle' })
  let loadGeneration = 0

  async function load(search: string, page = 1) {
    const generation = ++loadGeneration
    const query = search.trim()
    if (!query || query.length > 128 || !Number.isInteger(page) || page < 1) {
      users.value = query.length > 128
        ? { kind: 'error', diagnostic: makeDiagnostic('DIRECTORY_QUERY_TOO_LONG', '搜索内容不能超过 128 个字符。', false) }
        : { kind: 'idle' }
      return
    }
    users.value = { kind: 'loading', message: '查找组织账号…' }
    const result = await listOrganizationUsers({
      query: {
        query,
        page,
        pageSize: ORGANIZATION_DIRECTORY_PAGE_SIZE,
      },
    })
    if (generation !== loadGeneration) return
    if (result.error) {
      users.value = { kind: 'error', diagnostic: directoryDiagnostic(result.error) }
      return
    }
    users.value = { kind: 'success', data: result.data }
  }

  function clear() {
    loadGeneration += 1
    users.value = { kind: 'idle' }
  }

  onScopeDispose(() => { loadGeneration += 1 })

  return reactive({ users, load, clear })
}

export async function loadProject(projectId: string) {
  const result = await getProject({ path: { projectId } })
  if (result.error) throw result.error
  return result.data
}
