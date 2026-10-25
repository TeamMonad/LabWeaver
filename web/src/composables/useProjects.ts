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
  ProjectPageSchema,
  ProjectMembershipSchema,
  ProjectSchema,
  UpdateProjectRequestSchema,
} from '@/generated/contracts'
import { sessionEpoch } from '@/api/session'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

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
// A project the context asked to locate. Any reload that supersedes the
// locating one (another component mounting, a mutation) keeps following the
// pages for it until a load finishes.
let locateTarget: string | null = null

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

/**
 * Reload the catalog from its first page. The current project and `wanted`
 * may sit on a later page; pages are followed until they are listed or the
 * list ends, so the context is never replaced just because it was not on page one.
 */
async function loadProjects(wanted: string | null = null) {
  const generation = ++loadGeneration
  if (wanted) locateTarget = wanted
  projects.value = { kind: 'loading', message: '加载项目…' }
  loadingMoreProjects.value = false
  loadMoreProjectsError.value = null
  const targets = [selectedProjectId.value, locateTarget].filter((id): id is string => Boolean(id))
  let items: ProjectSchema[] = []
  let page: ProjectPageSchema = { items: [], nextCursor: null }
  do {
    const result = await listProjects({ query: pageQuery(page.nextCursor ?? null) })
    if (generation !== loadGeneration) return
    if (result.error) {
      locateTarget = null
      projectsNextCursor.value = null
      projects.value = { kind: 'error', diagnostic: diagnostic(result.error, 'PROJECT_LIST_FAILED', '加载项目失败') }
      settledProjects.value = projects.value
      return
    }
    page = result.data
    items = appendPage(items, page.items, (project) => project.id)
  } while (page.nextCursor && targets.some((id) => !items.some((project) => project.id === id)))
  locateTarget = null
  const cursor = page.nextCursor ?? null
  projectsNextCursor.value = cursor
  if (items.length === 0) {
    selectedProjectId.value = null
    projects.value = { kind: 'empty' }
    settledProjects.value = projects.value
    return
  }
  preserveSelection(items)
  projects.value = { kind: 'success', data: items }
  settledProjects.value = projects.value
}

/** Append the next server page to the loaded catalog. */
async function loadMoreProjects() {
  const cursor = projectsNextCursor.value
  const loaded = projects.value
  if (!cursor || loadingMoreProjects.value || loaded.kind !== 'success') return
  const generation = loadGeneration
  loadingMoreProjects.value = true
  loadMoreProjectsError.value = null
  const result = await listProjects({ query: pageQuery(cursor) })
  if (generation !== loadGeneration) return
  loadingMoreProjects.value = false
  if (result.error) {
    loadMoreProjectsError.value = diagnostic(result.error, 'PROJECT_LIST_MORE_FAILED', '加载更多项目失败')
    return
  }
  projectsNextCursor.value = result.data.nextCursor ?? null
  projects.value = { kind: 'success', data: appendPage(loaded.data, result.data.items, (project) => project.id) }
  settledProjects.value = projects.value
}

// The previous actor's list must not decide the new actor's project context:
// the AppShell waits for this actor's list before it renders a scoped page.
watch(sessionEpoch, () => {
  settledProjects.value = { kind: 'idle' }
  locateTarget = null
  void loadProjects()
})

/**
 * Project is the browser's durable context for Work, Agent and Resource calls.
 * The composable keeps only server projections; it never synthesizes a project
 * when the API is unavailable.
 */
export function useProjects() {
  // Template handlers pass DOM events; `load` always restarts without a locate target.
  async function load() {
    await loadProjects()
  }

  function select(projectId: string) {
    selectedProjectId.value = projectId
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

  async function load() {
    const id = projectId.value
    const generation = ++loadGeneration
    nextCursor.value = null
    loadingMore.value = false
    loadMoreError.value = null
    if (!id) {
      memberships.value = { kind: 'idle' }
      return
    }
    memberships.value = { kind: 'loading', message: '加载项目成员…' }
    const result = await listProjectMemberships({ path: { projectId: id }, query: pageQuery(null) })
    if (generation !== loadGeneration || projectId.value !== id) return
    if (result.error) {
      memberships.value = { kind: 'error', diagnostic: diagnostic(result.error, 'PROJECT_MEMBERS_LIST_FAILED', '加载项目成员失败') }
      return
    }
    nextCursor.value = result.data.nextCursor ?? null
    memberships.value = result.data.items.length > 0 ? { kind: 'success', data: result.data.items } : { kind: 'empty' }
  }

  async function loadMore() {
    const id = projectId.value
    const cursor = nextCursor.value
    const loaded = memberships.value
    if (!id || !cursor || loadingMore.value || loaded.kind !== 'success') return
    const generation = loadGeneration
    loadingMore.value = true
    loadMoreError.value = null
    const result = await listProjectMemberships({ path: { projectId: id }, query: pageQuery(cursor) })
    if (generation !== loadGeneration || projectId.value !== id) return
    loadingMore.value = false
    if (result.error) {
      loadMoreError.value = diagnostic(result.error, 'PROJECT_MEMBERS_LIST_MORE_FAILED', '加载更多项目成员失败')
      return
    }
    nextCursor.value = result.data.nextCursor ?? null
    memberships.value = { kind: 'success', data: appendPage(loaded.data, result.data.items, (membership) => membership.actorId) }
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
