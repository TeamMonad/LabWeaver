import { computed, inject, onMounted, provide, ref, watch, type InjectionKey, type Ref } from 'vue'
import { onBeforeRouteUpdate, useRoute, useRouter } from 'vue-router'
import { useEnvironmentTemplateReleases } from '@/composables/useEnvironmentTemplateReleases'
import { useEnvironmentInstance } from '@/composables/useEnvironmentInstance'
import { useEnvironmentLifecycle, useEnvironmentLifecycleControls } from '@/composables/useEnvironmentLifecycle'
import { useEnvironmentAccess } from '@/composables/useEnvironmentAccess'
import { useEnvironmentOperations } from '@/composables/useEnvironmentOperations'
import { useConnectionWatch } from '@/composables/useConnectionWatch'
import { useConsoleCapability } from '@/composables/useConsoleCapability'
import { useEnvironmentSubmission } from '@/composables/useEnvironmentSubmission'
import { useProjects } from '@/composables/useProjects'
import type { TimelineEvent } from '@/components/common/EventTimeline.vue'
import type { ConsoleKind, EnvironmentInstanceSchema, EnvironmentOperationSnapshotSchema, EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'
import { makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import { accessGrantBlockReason, canIssueAccessGrant, operationTimelineDescription } from '@/utils/environmentLifecycle'
import { grantIsUsable } from '@/utils/environmentConnection'
import { idempotencyKey } from '@/utils/format'
import { environmentOperationKindLabel, environmentOperationStateLabel } from '@/utils/stateLabels'

export type EnvironmentEntryTab = 'overview' | 'terminal' | 'operations' | 'freeze'

function latest(items: EnvironmentOperationSnapshotSchema[], keep: (op: EnvironmentOperationSnapshotSchema) => boolean) {
  return items.filter(keep).sort((left, right) => right.acceptedAt.localeCompare(left.acceptedAt))[0] ?? null
}

/**
 * Everything the environment console page shares between its parts: route and
 * project context, the selected environment, access, operations, lifecycle
 * controls and the experiment submission. The page provides it once; each
 * section component injects what it needs.
 */
export function useEnvironmentEntry(teacherMode: Ref<boolean>) {
  const route = useRoute()
  const router = useRouter()
  const projects = useProjects()

  const activeTab = ref<EnvironmentEntryTab>('overview')
  const selectedEnvironmentId = ref<string | undefined>(
    typeof route.query.environmentId === 'string' ? route.query.environmentId : undefined,
  )
  const environmentIdInput = ref(selectedEnvironmentId.value ?? '')
  const environmentToolsOpen = ref(false)
  const createDiagnostic = ref<DiagnosticViewModel | null>(null)
  const createGrantDiagnostic = ref<DiagnosticViewModel | null>(null)
  const consoleSessionKey = ref(0)

  const routeProjectId = computed(() => {
    const id = typeof route.query.projectId === 'string' ? route.query.projectId.trim() : ''
    return id || undefined
  })
  /** Work is created through a resource request on the Work list, never from this console. */
  const isWorkRoute = computed(() => route.path.startsWith('/researcher/'))
  const handoffProjectId = computed(() => isWorkRoute.value
    ? routeProjectId.value ?? projects.selectedProjectId ?? undefined
    : undefined)
  const env = useEnvironmentInstance(selectedEnvironmentId, handoffProjectId)
  const instance = computed<EnvironmentInstanceSchema | null>(() => env.instance.kind === 'success' ? env.instance.data : null)
  const isEnvironmentLoaded = computed(() => env.instance.kind === 'success')
  const environmentTitle = computed(() => {
    if (env.instance.kind === 'success') return env.instance.data.displayLabel
    if (env.instance.kind === 'loading') return '正在加载环境'
    return null
  })
  const environmentProjectId = computed(() => instance.value?.projectId)
  const projectId = computed(() => routeProjectId.value ?? instance.value?.projectId ?? projects.selectedProjectId ?? undefined)
  const selectedProject = computed(() => {
    const id = projectId.value
    if (!id || projects.projects.kind !== 'success') return null
    return projects.projects.data.find((project) => project.id === id) ?? null
  })
  const courseId = computed(() => instance.value ? instance.value.courseId ?? undefined : selectedProject.value?.courseId ?? undefined)
  const isContextMissing = computed(() => !projectId.value)
  const isWorkConnection = computed(() => isWorkRoute.value || instance.value?.class === 'work')
  const environmentListPath = computed(() => teacherMode.value
    ? '/teacher/environments'
    : isWorkConnection.value ? '/researcher/environments' : '/student/labs')
  const environmentListActionLabel = computed(() => teacherMode.value
    ? '查看项目环境'
    : isWorkConnection.value ? '返回 Work 列表' : '选择已有实验环境')
  const environmentEmptyHint = computed(() => teacherMode.value
    ? '请从教师项目环境列表选择已有环境，或从上方创建环境。'
    : isWorkConnection.value
      ? '请在 Work 列表中选择已有 Work，或选择模板新建 Work。'
      : '请从课程实验列表选择已有环境，或从上方已发布模板创建环境。')
  const breadcrumbLabel = computed(() => teacherMode.value
    ? '教师项目环境'
    : isWorkConnection.value ? 'Work 环境' : '课程实验环境')
  const environmentToolsLabel = computed(() => {
    if (isWorkRoute.value) return environmentToolsOpen.value ? '收起切换' : '按环境 ID 切换'
    return environmentToolsOpen.value ? '收起创建与切换' : '创建或切换环境'
  })

  const releases = useEnvironmentTemplateReleases(projectId, courseId)
  const lifecycle = useEnvironmentLifecycle(projectId, courseId)
  const access = useEnvironmentAccess(selectedEnvironmentId, computed(() => instance.value?.revision), projectId, courseId)

  // ---- Connection readiness and browser console ----------------------------

  const connection = useConnectionWatch(instance, access)
  const connectionRechecking = ref(false)
  async function recheckConnection() {
    connectionRechecking.value = true
    try {
      await connection.recheck()
    } finally {
      connectionRechecking.value = false
    }
  }
  /** A grant past its own expiry no longer opens anything, even before the server is read again. */
  const grantUsable = computed(() => grantIsUsable(access.grant, connection.now.value))
  const consoleCapability = useConsoleCapability(computed(() => (
    grantUsable.value && access.grant.kind === 'success' ? access.grant.data.id : undefined
  )))
  watch(
    () => grantUsable.value && access.grant.kind === 'success' ? `${access.grant.data.id}:${access.grant.data.revision}` : null,
    (identity) => {
      if (identity) void consoleCapability.load()
      else consoleCapability.reset()
    },
    { immediate: true },
  )
  const requestedConsoleKind = ref<ConsoleKind | null>(null)
  /** The console the user asked for, else the first one the grant permits; the panel itself rejects kinds the grant does not permit. */
  const consolePanelKind = computed<ConsoleKind>(() => {
    if (requestedConsoleKind.value) return requestedConsoleKind.value
    if (consoleCapability.availability.kind === 'success' && consoleCapability.availability.data.kinds.length > 0) {
      return consoleCapability.availability.data.kinds[0]
    }
    return instance.value?.runtimeKind === 'virtual_machine' ? 'novnc' : 'xterm'
  })
  function openConsoleTab(kind: ConsoleKind) {
    requestedConsoleKind.value = kind
    activeTab.value = 'terminal'
  }

  /**
   * "签发授权并连接" from the connection summary: issue the grant, then open the
   * first browser console the server permits for it. Without a permitted console
   * the user stays on the overview, where SSH and application entries are listed.
   */
  const connectAfterGrant = ref<string | null>(null)
  async function issueGrantAndConnect() {
    const environmentId = selectedEnvironmentId.value ?? null
    connectAfterGrant.value = environmentId
    await issueAccessGrant()
    if (!grantIsUsable(access.grant, Date.now())) connectAfterGrant.value = null
  }
  watch(
    () => consoleCapability.availability,
    (state) => {
      if (!connectAfterGrant.value || connectAfterGrant.value !== selectedEnvironmentId.value) return
      if (state.kind === 'idle' || state.kind === 'loading') return
      connectAfterGrant.value = null
      if (state.kind === 'success' && state.data.kinds.length > 0) openConsoleTab(state.data.kinds[0])
    },
  )

  const operations = useEnvironmentOperations(selectedEnvironmentId)

  const operationItems = computed(() => operations.operations.kind === 'success' ? operations.operations.data : [])
  const operationTimeline = computed<TimelineEvent[]>(() => operationItems.value.map((op) => ({
    id: op.operationId,
    title: `${environmentOperationKindLabel(op.kind)} · ${environmentOperationStateLabel(op.state)}`,
    timestamp: op.terminalAt ?? op.acceptedAt,
    description: operationTimelineDescription(op),
  })))
  const activeOperation = computed(() => latest(operationItems.value, (op) => op.state === 'accepted' || op.state === 'running' || op.state === 'cancelling'))
  const failedOperation = computed(() => latest(operationItems.value, (op) => op.state === 'failed'))
  const retryableOperation = computed(() => instance.value?.desiredState === 'deleted'
    ? null
    : latest(operationItems.value, (op) => op.state === 'failed' && op.retryEligible))

  const controls = useEnvironmentLifecycleControls({ env, operations, lifecycle, activeOperation, retryableOperation })
  const submission = useEnvironmentSubmission({ env, operations, releases, selectedEnvironmentId, isWorkConnection })

  async function refreshAll() {
    await Promise.all([env.load(), access.loadEndpoints(), operations.load()])
  }

  async function cancelCurrentOperation() {
    const operation = activeOperation.value
    const data = instance.value
    if (!operation || !data || data.id !== operation.environmentId) return
    const result = await operations.cancel(data.id, data.revision)
    if (result.ok) await Promise.all([env.load(), operations.load()])
  }

  // ---- Route and project context -----------------------------------------

  watch(
    () => route.query.environmentId,
    (q) => {
      selectedEnvironmentId.value = typeof q === 'string' ? q : undefined
      environmentIdInput.value = selectedEnvironmentId.value ?? ''
    },
  )

  watch(
    selectedEnvironmentId,
    (id) => {
      access.resetGrant()
      if (id) environmentToolsOpen.value = false
      controls.resetDiagnostics()
      createGrantDiagnostic.value = null
      if (id) void Promise.all([access.loadEndpoints(), access.loadCurrentGrant()])
    },
    { immediate: true },
  )

  function hasProject(id: string): boolean {
    return projects.projects.kind === 'success' && projects.projects.data.some((project) => project.id === id)
  }

  function clearEnvironmentSelection() {
    selectedEnvironmentId.value = undefined
    environmentIdInput.value = ''
    environmentToolsOpen.value = true
  }

  function syncEnvironmentToolsOpen(event: Event) {
    if (event.currentTarget instanceof HTMLDetailsElement) environmentToolsOpen.value = event.currentTarget.open
  }

  watch(
    [() => projects.projects, routeProjectId, environmentProjectId],
    ([state, requestedProjectId, instanceProjectId]) => {
      const contextProjectId = requestedProjectId ?? instanceProjectId
      if (state.kind !== 'success' || !contextProjectId || !state.data.some((project) => project.id === contextProjectId)) return
      if (projects.selectedProjectId !== contextProjectId) projects.select(contextProjectId)
    },
    { immediate: true },
  )

  const routeNavigationProjectId = ref<string | null>(null)

  onBeforeRouteUpdate((to) => {
    const nextProjectId = typeof to.query.projectId === 'string' ? to.query.projectId.trim() : ''
    const nextEnvironmentId = typeof to.query.environmentId === 'string' ? to.query.environmentId.trim() : ''
    if (nextProjectId !== (routeProjectId.value ?? '')) {
      const currentEnvironmentId = selectedEnvironmentId.value
      if (!nextEnvironmentId || nextEnvironmentId === currentEnvironmentId) clearEnvironmentSelection()
      if (nextEnvironmentId === currentEnvironmentId && to.query.environmentId !== undefined) {
        return { query: { ...to.query, environmentId: undefined } }
      }
    }
    if (nextProjectId && hasProject(nextProjectId) && projects.selectedProjectId !== nextProjectId) {
      routeNavigationProjectId.value = nextProjectId
      projects.select(nextProjectId)
    }
    return true
  })

  function syncSelectedProjectToRoute(selectedId: string | null, previousId: string | null = null) {
    if (!selectedId || !hasProject(selectedId)) return
    if (routeNavigationProjectId.value === selectedId) {
      routeNavigationProjectId.value = null
      return
    }
    const contextProjectId = routeProjectId.value ?? environmentProjectId.value
    if (routeProjectId.value && !hasProject(routeProjectId.value)) return
    if (contextProjectId === selectedId) return
    if (selectedEnvironmentId.value && !environmentProjectId.value && (env.instance.kind === 'idle' || env.instance.kind === 'loading') && previousId === null) return
    clearEnvironmentSelection()
    void router.replace({ query: { ...route.query, projectId: selectedId, environmentId: undefined } })
  }

  // Wait until this route has mounted before synchronizing project context. A
  // project list can resolve while the router is still running the auth guard;
  // replacing from setup in that window would use the previous route (often the
  // root route) and cancel the user's navigation. Mounting is the router's
  // committed route boundary, so the same synchronization remains immediate for
  // this page without racing a pending navigation.
  onMounted(() => {
    if (selectedEnvironmentId.value) {
      watch(() => projects.selectedProjectId, syncSelectedProjectToRoute)
      return
    }
    watch(
      [() => projects.selectedProjectId, () => projects.projects],
      ([selectedId], [previousId]) => syncSelectedProjectToRoute(selectedId, previousId),
      { immediate: true },
    )
  })

  watch(
    () => instance.value ? `${instance.value.id}:${instance.value.revision}:${instance.value.observedState}` : null,
    (identity, previousIdentity) => {
      if (identity && identity !== previousIdentity) void Promise.all([access.loadEndpoints(), access.loadCurrentGrant()])
    },
  )

  function applyEnvironmentId() {
    const id = environmentIdInput.value.trim()
    if (!id) return
    router.replace({ query: { ...route.query, environmentId: id } })
  }

  // ---- Create from a published release -----------------------------------

  // One Idempotency-Key per create intent (release + user), kept across retries
  // until the intent reaches a terminal outcome, so a timeout + retry replays the
  // same create instead of minting a duplicate environment.
  const pendingRelease = ref<EnvironmentTemplateReleaseViewSchema | null>(null)
  const createIntentKey = ref<string | null>(null)
  const createIntentReleaseId = ref<string | null>(null)

  async function createFromRelease(release: EnvironmentTemplateReleaseViewSchema) {
    createDiagnostic.value = null
    pendingRelease.value = release
    if (createIntentReleaseId.value !== release.id || !createIntentKey.value) {
      createIntentKey.value = idempotencyKey()
      createIntentReleaseId.value = release.id
    }
    const intentKey = createIntentKey.value
    lifecycle.operating.add(`create:${release.id}`)
    try {
      const result = await lifecycle.create({ releaseId: release.id, releaseVersion: release.version }, intentKey)
      if (!result.ok) {
        createDiagnostic.value = result.diagnostic
      } else {
        createIntentKey.value = null
        createIntentReleaseId.value = null
        if (result.accepted?.environmentId) {
          router.replace({ query: { ...route.query, environmentId: result.accepted.environmentId } })
        }
      }
    } finally {
      lifecycle.operating.delete(`create:${release.id}`)
    }
  }

  function retryCreate() {
    if (pendingRelease.value) createFromRelease(pendingRelease.value)
  }

  // ---- Access grant and console ------------------------------------------

  function accessGrantRequestEnvironment(): EnvironmentInstanceSchema | null {
    const data = instance.value
    if (!data) {
      createGrantDiagnostic.value = makeDiagnostic('ACCESS_GRANT_ENVIRONMENT_NOT_READY', '环境状态尚未加载，无法签发访问授权。', false)
      return null
    }
    if (!canIssueAccessGrant(data)) {
      createGrantDiagnostic.value = makeDiagnostic('ACCESS_GRANT_NOT_ELIGIBLE', accessGrantBlockReason(data), false)
      return null
    }
    return data
  }

  async function issueAccessGrant() {
    if (!accessGrantRequestEnvironment()) return
    createGrantDiagnostic.value = null
    const result = await access.createGrant()
    if (result && !result.ok && result.diagnostic) createGrantDiagnostic.value = result.diagnostic
  }

  async function reconnectConsole() {
    createGrantDiagnostic.value = null
    if (grantIsUsable(access.grant, Date.now())) {
      consoleSessionKey.value += 1
      submission.consoleReconnectRequired.value = false
      return
    }
    if (!accessGrantRequestEnvironment()) return
    await issueAccessGrant()
    if (grantIsUsable(access.grant, Date.now())) {
      consoleSessionKey.value += 1
      submission.consoleReconnectRequired.value = false
    }
  }

  async function revokeAccessGrant() {
    createGrantDiagnostic.value = null
    const result = await access.revokeGrant()
    if (!result.ok && result.diagnostic) createGrantDiagnostic.value = result.diagnostic
  }

  return {
    teacherMode,
    activeTab,
    selectedEnvironmentId,
    environmentIdInput,
    environmentToolsOpen,
    createDiagnostic,
    createGrantDiagnostic,
    consoleSessionKey,
    env,
    instance,
    isEnvironmentLoaded,
    environmentTitle,
    projectId,
    isContextMissing,
    isWorkRoute,
    isWorkConnection,
    environmentListPath,
    environmentListActionLabel,
    environmentEmptyHint,
    breadcrumbLabel,
    environmentToolsLabel,
    releases,
    lifecycle,
    access,
    connection,
    connectionRechecking,
    recheckConnection,
    grantUsable,
    consoleCapability,
    consolePanelKind,
    openConsoleTab,
    issueGrantAndConnect,
    operations,
    operationTimeline,
    activeOperation,
    failedOperation,
    retryableOperation,
    refreshAll,
    cancelCurrentOperation,
    syncEnvironmentToolsOpen,
    applyEnvironmentId,
    createFromRelease,
    retryCreate,
    issueAccessGrant,
    reconnectConsole,
    revokeAccessGrant,
    ...controls,
    ...submission,
  }
}

export type EnvironmentEntryContext = ReturnType<typeof useEnvironmentEntry>

const ENVIRONMENT_ENTRY: InjectionKey<EnvironmentEntryContext> = Symbol('environment-entry')

export function provideEnvironmentEntry(context: EnvironmentEntryContext) {
  provide(ENVIRONMENT_ENTRY, context)
}

export function useEnvironmentEntryContext(): EnvironmentEntryContext {
  const context = inject(ENVIRONMENT_ENTRY)
  if (!context) throw new Error('useEnvironmentEntryContext() needs EnvironmentEntryView as an ancestor.')
  return context
}
