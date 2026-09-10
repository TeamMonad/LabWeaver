import { computed, onMounted, reactive, ref } from 'vue'
import {
  addProjectMembership,
  archiveProject,
  createProject,
  getProject,
  listProjectMemberships,
  listProjects,
  removeProjectMembership,
  updateProject,
} from '@/generated/contracts'
import type {
  AddProjectMembershipRequestSchema,
  ProjectMembershipSchema,
  ProjectSchema,
  UpdateProjectRequestSchema,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

function diagnostic(error: unknown, fallbackCode: string, fallbackMessage: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackMessage, problem?.retryable ?? true)
}

export interface ProjectMutationResult {
  kind: 'success' | 'error'
  diagnostic: DiagnosticViewModel
}

const projects = ref<AsyncState<ProjectSchema[]>>({ kind: 'idle' })
const selectedProjectId = ref<string | null>(null)
const acting = ref<string | null>(null)
const outcome = ref<ProjectMutationResult | null>(null)
let loadGeneration = 0

const selectedProject = computed<ProjectSchema | null>(() => {
  if (projects.value.kind !== 'success' || !selectedProjectId.value) return null
  return projects.value.data.find((project) => project.id === selectedProjectId.value) ?? null
})

function preserveSelection(items: ProjectSchema[]) {
  if (items.length === 0) {
    selectedProjectId.value = null
    return
  }
  const saved = typeof localStorage !== 'undefined' ? localStorage.getItem('labweaver_project_id') : null
  if (!selectedProjectId.value || !items.some((project) => project.id === selectedProjectId.value)) {
    selectedProjectId.value = saved && items.some((project) => project.id === saved) ? saved : items[0].id
  }
}

async function loadProjects() {
  const generation = ++loadGeneration
  projects.value = { kind: 'loading', message: '加载项目…' }
  const result = await listProjects({})
  if (generation !== loadGeneration) return
  if (result.error) {
    projects.value = { kind: 'error', diagnostic: diagnostic(result.error, 'PROJECT_LIST_FAILED', '加载项目失败') }
    return
  }
  if (result.data.length === 0) {
    selectedProjectId.value = null
    projects.value = { kind: 'empty' }
    return
  }
  projects.value = { kind: 'success', data: result.data }
  preserveSelection(result.data)
}

/**
 * Project is the browser's durable context for Work, Agent and Resource calls.
 * The composable keeps only server projections; it never synthesizes a project
 * when the API is unavailable.
 */
export function useProjects() {
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
    selectedProjectId,
    selectedProject,
    acting,
    outcome,
    load,
    select,
    create,
    update,
    archive,
  })
}

/** Load and mutate project-local Access memberships. */
export function useProjectMemberships(projectId: ReturnType<typeof ref<string | null>>) {
  const memberships = ref<AsyncState<ProjectMembershipSchema[]>>({ kind: 'idle' })
  const acting = ref<string | null>(null)
  const outcome = ref<ProjectMutationResult | null>(null)

  async function load() {
    const id = projectId.value
    if (!id) {
      memberships.value = { kind: 'idle' }
      return
    }
    memberships.value = { kind: 'loading', message: '加载项目成员…' }
    const result = await listProjectMemberships({ path: { projectId: id } })
    if (result.error) {
      memberships.value = { kind: 'error', diagnostic: diagnostic(result.error, 'PROJECT_MEMBERS_LIST_FAILED', '加载项目成员失败') }
      return
    }
    memberships.value = result.data.length > 0 ? { kind: 'success', data: result.data } : { kind: 'empty' }
  }

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

  return reactive({ memberships, acting, outcome, load, add, remove })
}

export async function loadProject(projectId: string) {
  const result = await getProject({ path: { projectId } })
  if (result.error) throw result.error
  return result.data
}
