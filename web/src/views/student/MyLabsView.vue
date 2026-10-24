<template>
  <div class="my-labs">
    <header class="page-header">
      <div class="header-title-row">
        <h2>{{ props.teacherMode ? '项目环境' : '我的项目环境' }}</h2>
        <span class="header-badge" v-if="state.kind === 'success'">{{ state.data.length }} 个环境</span>
      </div>
      <p class="page-subtitle">
        {{ props.teacherMode
          ? '查看当前项目下的课程实验和 Work 环境；进入控制台管理运行状态与访问授权。'
          : '查看当前项目的环境：先选择实验模板提交资源申请，审批通过后进入环境；进入终端控制台或提交评测材料。' }}
      </p>
    </header>

    <DiagnosticBanner
      v-if="isInvalidProjectContext"
      code="PROJECT_CONTEXT_INVALID"
      message="链接中的项目当前不可访问，请通过项目选择器选择可访问的项目。"
      :retryable="false"
      severity="error"
    />

    <DiagnosticBanner
      v-else-if="isContextMissing"
      code="PROJECT_CONTEXT_MISSING"
      :message="props.teacherMode
        ? '项目上下文未绑定，无法加载项目环境。请通过顶栏项目选择器选择项目或联系管理员。'
        : '项目上下文未绑定，无法加载你的实验列表。请通过顶栏项目选择器选择项目或联系管理员。'"
      :retryable="false"
      severity="error"
    />

    <div
      v-else-if="isProjectContextLoading"
      class="project-context-loading"
      role="status"
      aria-live="polite"
    >
      正在同步项目上下文，请稍候…
    </div>

    <template v-else>
      <!-- GCP Action Bar -->
      <GcpActionBar
        :refreshing="state.kind === 'loading'"
        :show-auto-refresh="true"
        :auto-refresh="autoRefreshEnabled"
        aria-label="项目环境管理工具栏"
        @refresh="load"
        @update:auto-refresh="autoRefreshEnabled = $event"
      >
        <template #leading>
          <button
            v-if="!props.teacherMode"
            type="button"
            class="filled-button"
            @click="openCreateDrawer"
          >
            <SvgIcon name="add" size="sm" aria-hidden="true" />
            <span>申请实验环境</span>
          </button>
        </template>
      </GcpActionBar>

      <section
        v-if="!props.teacherMode"
        class="request-section md-card"
        aria-labelledby="experiment-requests-heading"
        data-testid="experiment-requests"
      >
        <div class="request-section__head">
          <h3 id="experiment-requests-heading">实验资源申请</h3>
          <RouterLink class="text-button small" :to="resourceRequestLink()">查看全部资源申请</RouterLink>
        </div>
        <DiagnosticBanner
          v-if="requestSectionDiagnostic"
          :diagnostic="requestSectionDiagnostic"
          :message="`无法读取实验资源申请，可能有已批准但尚未进入的环境：${requestSectionDiagnostic.message}`"
          severity="warning"
          @retry="reloadRequests"
        />

        <h4 class="request-section__subtitle">已批准的申请</h4>
        <ul v-if="approvedRequests.length > 0" class="request-list" data-testid="approved-requests">
          <li
            v-for="request in approvedRequests"
            :key="request.id"
            class="request-row"
            :data-request-id="request.id"
          >
            <div class="request-row__main">
              <strong>{{ requestReleaseTitle(request) }}</strong>
              <span class="request-row__meta">{{ resourceSummary(request.requestedResources) }} · 申请于 {{ formatTimestamp(request.createdAt) }}</span>
              <span
                v-if="attachOutcomes[request.id]?.kind === 'preparing'"
                class="request-row__meta"
                role="status"
                data-testid="environment-preparing"
              >环境准备中：申请已批准，平台正在分配资源并创建环境。请稍候再点击“进入环境”，不会重复创建。</span>
            </div>
            <button
              type="button"
              class="filled-button small"
              :disabled="attaching.has(request.id) || !actorId"
              @click="enterEnvironment(request)"
            >
              {{ attaching.has(request.id) ? '正在进入…' : '进入环境' }}
            </button>
            <DiagnosticBanner
              v-if="attachFailure(request.id)"
              class="request-row__diagnostic"
              :diagnostic="attachFailure(request.id)"
              @retry="enterEnvironment(request)"
            />
          </li>
        </ul>
        <p v-else class="request-section__hint" data-testid="approved-requests-empty">
          还没有可以进入的已批准申请。请点击“申请实验环境”，选择实验模板并提交资源申请；管理员审批通过后，在这里进入环境。
        </p>

        <template v-if="reviewingRequests.length > 0">
          <h4 class="request-section__subtitle">待审批</h4>
          <ul class="request-list" data-testid="reviewing-requests">
            <li v-for="request in reviewingRequests" :key="request.id" class="request-row" :data-request-id="request.id">
              <div class="request-row__main">
                <strong>{{ requestReleaseTitle(request) }}</strong>
                <span class="request-row__meta">{{ resourceSummary(request.requestedResources) }} · 申请于 {{ formatTimestamp(request.createdAt) }} · 等待管理员审批</span>
              </div>
              <RouterLink class="outlined-button small" :to="resourceRequestLink()">查看申请</RouterLink>
            </li>
          </ul>
        </template>

        <template v-if="rejectedRequests.length > 0">
          <h4 class="request-section__subtitle">已拒绝</h4>
          <ul class="request-list" data-testid="rejected-requests">
            <li v-for="request in rejectedRequests" :key="request.id" class="request-row" :data-request-id="request.id">
              <div class="request-row__main">
                <strong>{{ requestReleaseTitle(request) }}</strong>
                <span class="request-row__meta request-row__meta--error">{{ rejectionReason(request) }}</span>
              </div>
              <RouterLink class="outlined-button small" :to="resourceRequestLink(request.target)">重新申请</RouterLink>
            </li>
          </ul>
        </template>
      </section>

      <!-- GCP Filter Bar -->
      <GcpFilterBar
        v-if="state.kind === 'success'"
        v-model="filterSearch"
        placeholder="按项目环境名称、环境 ID 或 Runtime 过滤…"
        :presets="filterPresets"
        @filter-change="onFilterChange"
      />

      <AsyncStateView :state="state" @retry="load">
        <template #success>
          <DataTable
            :columns="columns"
            :rows="filteredRows"
            empty-text="没有匹配过滤条件的项目环境"
            interactive
            :aria-label="props.teacherMode ? '项目环境列表' : '我的项目环境列表'"
            @row-click="(row) => selectRowForInspect(row as unknown as EnvironmentSummary)"
          >
            <template #displayLabel="{ row }">
              <div class="env-name-cell">
                <span class="env-name">{{ row.displayLabel }}</span>
                <span class="env-kind-sub">{{ environmentClassLabel(row.class) }}</span>
                <span class="env-id-sub">{{ row.id }}</span>
              </div>
            </template>

            <template #observedState="{ row }">
              <div class="state-cell">
                <GcpStatusPill
                  :state="row.observedState"
                  domain="environment"
                />
                <small class="env-next-action">{{ nextActionLabel(row) }}</small>
              </div>
            </template>

            <template #runtimeKind="{ row }">
              <span class="runtime-tag">
                <SvgIcon :name="row.runtimeKind === 'container' ? 'inventory_2' : 'dns'" size="sm" aria-hidden="true" />
                <span>{{ row.runtimeKind === 'container' ? '容器' : '虚拟机' }}</span>
              </span>
            </template>

            <template #eligibilityExpiresAt="{ row }">
              {{ environmentMaterialDeadlineLabel(row.eligibilityExpiresAt) }}
            </template>

            <template #actions="{ row }">
              <div class="row-actions" @click.stop>
                <button
                  type="button"
                  class="filled-button small"
                  :disabled="!canOpenConsole(row)"
                  :title="consoleActionReason(row)"
                  :aria-label="canOpenConsole(row) ? '打开控制台' : '控制台不可用'"
                  @click="openEnvironment(row.id)"
                >
                  <SvgIcon name="terminal" size="sm" aria-hidden="true" />
                  <span>{{ canOpenConsole(row) ? '控制台' : '控制台不可用' }}</span>
                </button>
                <button
                  v-if="failedCleanup(row)"
                  type="button"
                  class="outlined-button small"
                  @click="openCleanup(row)"
                >
                  处理回收
                </button>
                <button
                  type="button"
                  class="outlined-button small"
                  @click="selectRowForInspect(row)"
                >
                  详情
                </button>
              </div>
            </template>
          </DataTable>

          <p class="labs-hint" role="status">
            {{ props.teacherMode
              ? '点击行可查看项目环境规格与端点详情；进入控制台后可管理生命周期和访问授权。'
              : '点击行可原地查看项目环境规格与端点详情；新环境需要先通过「申请实验环境」提交资源申请，审批通过后在「已批准的申请」中进入。' }}
          </p>
        </template>

        <template #empty>
          <div class="empty-labs-pane">
            <SvgIcon name="science" size="xl" aria-hidden="true" />
            <h3>暂无项目环境</h3>
            <p>{{ props.teacherMode ? '当前项目还没有可管理的环境。' : '当前项目还没有环境。请先选择教师已发布的实验模板提交资源申请，审批通过后在上方“已批准的申请”中进入环境。' }}</p>
            <button
              v-if="!props.teacherMode"
              type="button"
              class="filled-button"
              @click="openCreateDrawer"
            >
              <SvgIcon name="add" size="sm" aria-hidden="true" />
              <span>申请实验环境</span>
            </button>
          </div>
        </template>
      </AsyncStateView>
    </template>

    <!-- Right Side-Sheet Inspector (GCP Style) -->
    <EvidenceSideSheet
      :open="inspectedEnv !== null"
      title="项目环境详情"
      @close="inspectedEnv = null"
    >
      <div v-if="inspectedEnv" class="inspect-content">
        <div class="inspect-header">
          <h4>{{ inspectedEnv.displayLabel }}</h4>
          <GcpStatusPill :state="inspectedEnv.observedState" domain="environment" />
        </div>

        <div class="inspect-properties">
          <div class="prop-row">
            <span class="prop-label">项目环境 ID</span>
            <div class="prop-value-with-copy">
              <code>{{ inspectedEnv.id }}</code>
              <CopyButton :text="inspectedEnv.id" label="复制环境 ID" />
            </div>
          </div>
          <div class="prop-row">
            <span class="prop-label">Runtime 类型</span>
            <span class="prop-value">{{ inspectedEnv.runtimeKind === 'container' ? '容器环境 (Container)' : '虚拟机环境 (KubeVirt VM)' }}</span>
          </div>
          <div class="prop-row">
            <span class="prop-label">用途</span>
            <span class="prop-value">{{ environmentClassLabel(inspectedEnv.class) }}</span>
          </div>
          <div class="prop-row">
            <span class="prop-label">材料保留至</span>
            <span class="prop-value">{{ environmentMaterialDeadlineLabel(inspectedEnv.eligibilityExpiresAt) }}</span>
          </div>
          <p class="inspect-next-action" role="status">{{ nextActionLabel(inspectedEnv) }}</p>
        </div>

        <div class="inspect-actions">
          <button
            type="button"
            class="filled-button full-width"
            :disabled="!canOpenConsole(inspectedEnv)"
            :title="consoleActionReason(inspectedEnv)"
            @click="openEnvironment(inspectedEnv.id)"
          >
            <SvgIcon name="terminal" size="sm" aria-hidden="true" />
            <span>{{ canOpenConsole(inspectedEnv) ? '进入终端控制台' : '终端控制台不可用' }}</span>
          </button>
          <button
            v-if="failedCleanup(inspectedEnv)"
            type="button"
            class="outlined-button full-width"
            @click="openCleanup(inspectedEnv)"
          >
            处理回收
          </button>
        </div>
      </div>
    </EvidenceSideSheet>

    <!-- Experiment template selector: choosing one opens the resource request form for it -->
    <div
      v-if="showCreateModal && !props.teacherMode"
      class="create-modal-overlay"
      role="dialog"
      aria-label="选择实验模板"
      @click.self="showCreateModal = false"
    >
      <div class="create-modal md-card">
        <div class="modal-header">
          <div class="modal-title-group">
            <SvgIcon name="add_circle" size="md" class="modal-icon" aria-hidden="true" />
            <h3>选择实验模板</h3>
          </div>
          <button
            type="button"
            class="icon-button"
            aria-label="关闭选择弹窗"
            @click="showCreateModal = false"
          >
            <SvgIcon name="close" size="md" aria-hidden="true" />
          </button>
        </div>

        <div class="modal-body">
          <AsyncStateView :state="experimentReleaseState" @retry="releases.load">
            <template #success="{ data: releaseList }">
              <p class="modal-subtitle">选择教师已发布的实验模板，然后提交资源申请；资源规格请按实验说明填写，审批通过后回到本页进入环境。</p>
              <div class="release-cards-grid">
                <div
                  v-for="rel in releaseList"
                  :key="`${rel.id}:${rel.version}`"
                  class="release-card"
                  :data-release-id="rel.id"
                >
                  <div class="release-card__top">
                    <span class="release-runtime-badge" :class="`badge--${rel.runtimeKind}`">
                      <SvgIcon :name="rel.runtimeKind === 'container' ? 'inventory_2' : 'dns'" size="sm" aria-hidden="true" />
                      {{ rel.runtimeKind === 'container' ? '容器' : '虚拟机' }}
                    </span>
                    <strong class="release-title">{{ workTemplateTitle(rel) }}</strong>
                    <code class="release-id">{{ rel.id }}</code>
                  </div>
                  <div class="release-meta">
                    <span>发布者：{{ rel.publishedBy }}</span>
                    <span>发布时间：{{ formatTimestamp(rel.publishedAt) }}</span>
                  </div>
                  <RouterLink class="filled-button small" :to="resourceRequestLink({ releaseId: rel.id, releaseVersion: rel.version })">
                    选择并申请资源
                  </RouterLink>
                </div>
              </div>
            </template>
            <template #empty>
              <div class="empty-release-note">
                <SvgIcon name="info" size="lg" aria-hidden="true" />
                <p>当前课程暂无已发布的实验模板，请联系课程教师在教师工作台中发布模板。</p>
              </div>
            </template>
          </AsyncStateView>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onScopeDispose, ref, watch } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import { useAuth } from '@/composables/useAuth'
import { useEnvironmentTemplateReleases } from '@/composables/useEnvironmentTemplateReleases'
import { useEnvironmentLifecycle } from '@/composables/useEnvironmentLifecycle'
import { resourceSummary, useProjectResources } from '@/composables/useProjectResources'
import { fetchProjectEnvironments } from '@/composables/useProjectWorkEnvironments'
import { useProjects } from '@/composables/useProjects'
import type { EnvironmentSummary, EnvironmentTemplateReleaseViewSchema, ResourceRequestSchema } from '@/generated/contracts'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DataTable from '@/components/common/DataTable.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import EvidenceSideSheet from '@/components/common/EvidenceSideSheet.vue'
import CopyButton from '@/components/common/CopyButton.vue'
import GcpStatusPill from '@/components/common/GcpStatusPill.vue'
import GcpActionBar from '@/components/common/GcpActionBar.vue'
import GcpFilterBar, { type FilterChip, type FilterPreset } from '@/components/common/GcpFilterBar.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { formatTimestamp, idempotencyKey } from '@/utils/format'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import type { DataTableColumn } from '@/components/common/DataTable.vue'
import { actorIdFromProfile } from '@/utils/navigation'
import { clearPendingCommand, readPendingCommand, writePendingCommand } from '@/utils/pendingCommands'
import { environmentStateLabel, resourceAllocationFailureMessage } from '@/utils/stateLabels'
import { workTemplateTitle } from '@/utils/workTemplate'

const props = withDefaults(defineProps<{
  /** Route used to open the shared environment control console. */
  environmentPath?: string
  /** Teacher inventory shows all classes in the selected project and does not create environments. */
  teacherMode?: boolean
}>(), {
  environmentPath: '/student/environments',
  teacherMode: false,
})

const projects = useProjects()
const route = useRoute()
const routeProjectId = computed(() => {
  const id = typeof route.query.projectId === 'string' ? route.query.projectId.trim() : ''
  return id || null
})
const routeProjectInvalid = computed(() => {
  if (!routeProjectId.value || projects.projects.kind !== 'success') return false
  return !projects.projects.data.some((project) => project.id === routeProjectId.value)
})
const routeProjectPending = computed(() => Boolean(
  routeProjectId.value
  && projects.projects.kind === 'success'
  && projects.selectedProjectId !== routeProjectId.value,
))
const projectContextBlocked = computed(() => projects.projects.kind !== 'success' || routeProjectInvalid.value || routeProjectPending.value)
const projectId = computed(() => projectContextBlocked.value ? undefined : projects.selectedProjectId ?? undefined)
const selectedProject = computed(() => projectContextBlocked.value ? null : projects.selectedProject)
const courseId = computed(() => selectedProject.value?.courseId ?? undefined)
const isInvalidProjectContext = computed(() => routeProjectInvalid.value)
const isProjectContextLoading = computed(() => {
  if (projects.projects.kind === 'idle' || projects.projects.kind === 'loading') return true
  if (projects.projects.kind !== 'success') return false
  if (routeProjectPending.value) return true
  return projects.projects.data.length > 0 && !projects.selectedProjectId
})
const isContextMissing = computed(() => !isInvalidProjectContext.value && !isProjectContextLoading.value && !projectId.value)

const router = useRouter()
const auth = useAuth()
const actorId = computed(() => actorIdFromProfile(auth.user.value?.profile) ?? null)
const releases = useEnvironmentTemplateReleases(projectId, courseId)
const lifecycle = useEnvironmentLifecycle(projectId, courseId)
// The teacher inventory never enters a student's request, so it reads none.
const resources = useProjectResources(computed(() => props.teacherMode ? null : projectId.value ?? null))

const state = ref<AsyncState<EnvironmentSummary[]>>({ kind: 'idle' })
const autoRefreshEnabled = ref(true)
let pollTimer: ReturnType<typeof setTimeout> | null = null
let loadGeneration = 0

const inspectedEnv = ref<EnvironmentSummary | null>(null)
const showCreateModal = ref(false)

type EnvironmentRequest = ResourceRequestSchema & { target: Extract<ResourceRequestSchema['target'], { kind: 'environment' }> }
type AttachOutcome = { kind: 'preparing' } | { kind: 'failed'; diagnostic: DiagnosticViewModel }

/** Experiment versions the server lists for this course project that can still be requested and entered. */
const experimentReleases = computed(() => releases.releases.kind === 'success'
  ? releases.releases.data.filter((release) => release.purpose === 'experiment' && !release.withdrawal)
  : [])
const experimentReleaseState = computed<AsyncState<EnvironmentTemplateReleaseViewSchema[]>>(() => {
  const current = releases.releases
  if (current.kind !== 'success') return current
  return experimentReleases.value.length > 0 ? { kind: 'success', data: experimentReleases.value } : { kind: 'empty' }
})

function requestRelease(request: EnvironmentRequest): EnvironmentTemplateReleaseViewSchema | undefined {
  return experimentReleases.value.find((release) => (
    release.id === request.target.releaseId && release.version === request.target.releaseVersion
  ))
}

/** The signed-in student's own environment requests bound to an experiment version of this project. */
const experimentRequests = computed<EnvironmentRequest[]>(() => {
  if (resources.requests.kind !== 'success') return []
  const actor = actorId.value
  return resources.requests.data.filter((request): request is EnvironmentRequest => (
    request.target.kind === 'environment'
    && request.projectId === projectId.value
    && request.requesterId === actor
    && requestRelease(request as EnvironmentRequest) !== undefined
  ))
})

/** Approved request states: Resource is allocating or has handed off the environment. */
const APPROVED_REQUEST_STATES = new Set<ResourceRequestSchema['state']>(['allocating', 'active', 'expiring'])

/**
 * Approved requests whose environment is not in the environment list yet.
 * Only offered once the list was actually read, so a listed environment is
 * never offered a second entry.
 */
const approvedRequests = computed(() => {
  const listed = state.value.kind === 'success'
    ? new Set(state.value.data.map((environment) => environment.id))
    : state.value.kind === 'empty' ? new Set<string>() : null
  if (!listed) return []
  return experimentRequests.value.filter((request) => (
    APPROVED_REQUEST_STATES.has(request.state) && !listed.has(request.target.environmentId)
  ))
})
const reviewingRequests = computed(() => experimentRequests.value.filter((request) => request.state === 'reviewing'))
const rejectedRequests = computed(() => experimentRequests.value.filter((request) => request.state === 'rejected'))

const requestSectionDiagnostic = computed<DiagnosticViewModel | null>(() => {
  for (const current of [resources.requests, releases.releases]) {
    if (current.kind !== 'success' && current.kind !== 'empty' && current.kind !== 'idle' && current.kind !== 'loading') return current.diagnostic
  }
  return null
})

const attaching = ref(new Set<string>())
const attachOutcomes = ref<Record<string, AttachOutcome>>({})

function setAttachOutcome(requestId: string, outcome: AttachOutcome | null) {
  const next = { ...attachOutcomes.value }
  if (outcome) next[requestId] = outcome
  else delete next[requestId]
  attachOutcomes.value = next
}

function attachFailure(requestId: string): DiagnosticViewModel | null {
  const outcome = attachOutcomes.value[requestId]
  return outcome?.kind === 'failed' ? outcome.diagnostic : null
}

const filterSearch = ref('')
const activeFilterChips = ref<FilterChip[]>([])

const filterPresets: FilterPreset[] = [
  { label: '运行中', key: 'observedState', value: 'ready', displayValue: '运行中' },
  { label: '置备中', key: 'observedState', value: 'provisioning', displayValue: '置备中' },
  { label: '失败', key: 'observedState', value: 'failed', displayValue: '失败' },
  { label: '容器', key: 'runtimeKind', value: 'container', displayValue: '容器' },
  { label: '虚拟机', key: 'runtimeKind', value: 'virtual_machine', displayValue: '虚拟机' },
]

const columns: DataTableColumn<EnvironmentSummary & { actions?: never }>[] = [
  { key: 'displayLabel', title: '项目环境名称 / ID' },
  { key: 'observedState', title: '状态' },
  { key: 'runtimeKind', title: 'Runtime' },
  { key: 'eligibilityExpiresAt', title: '材料保留至' },
  { key: 'actions', title: '操作' },
]

const filteredRows = computed(() => {
  if (state.value.kind !== 'success') return []
  let list = state.value.data

  const q = filterSearch.value.trim().toLowerCase()
  if (q) {
    list = list.filter(
      (r) =>
        r.displayLabel.toLowerCase().includes(q) ||
        r.id.toLowerCase().includes(q) ||
        r.runtimeKind.toLowerCase().includes(q) ||
        r.observedState.toLowerCase().includes(q),
    )
  }

  for (const chip of activeFilterChips.value) {
    if (chip.key === 'observedState') {
      list = list.filter((r) => r.observedState === chip.value)
    } else if (chip.key === 'runtimeKind') {
      list = list.filter((r) => r.runtimeKind === chip.value)
    }
  }

  return list
})

function onFilterChange(payload: { search: string; chips: FilterChip[] }) {
  filterSearch.value = payload.search
  activeFilterChips.value = payload.chips
}

function selectRowForInspect(env: EnvironmentSummary) {
  inspectedEnv.value = env
}

function environmentClassLabel(environmentClass: EnvironmentSummary['class']): string {
  return environmentClass === 'work' ? 'Work 项目环境' : '课程实验环境'
}

function environmentMaterialDeadlineLabel(deadline: string | null): string {
  return deadline === null ? '不过期，直到明确撤回' : formatTimestamp(deadline)
}

function hasActiveOperation(env: EnvironmentSummary): boolean {
  const operation = env.currentOperation
  return Boolean(operation && ['accepted', 'running', 'cancelling'].includes(operation.state))
}

function canOpenConsole(env: EnvironmentSummary): boolean {
  return env.desiredState !== 'deleted' && env.observedState !== 'deleting' && env.observedState !== 'deleted'
}

function failedCleanup(env: EnvironmentSummary): boolean {
  return env.desiredState === 'deleted'
    && env.observedState === 'failed'
    && env.currentOperation?.state === 'failed'
    && ['delete', 'expire', 'cleanup', 'cancel'].includes(env.currentOperation.kind)
}

function openCleanup(env: EnvironmentSummary) {
  if (!failedCleanup(env)) return
  void router.push({
    path: props.environmentPath,
    query: { environmentId: env.id, projectId: env.projectId },
  })
}

function consoleActionReason(env: EnvironmentSummary): string {
  if (env.observedState === 'deleted') return '此项目环境已删除，无法打开控制台。请创建新的项目环境。'
  if (env.observedState === 'deleting' || env.desiredState === 'deleted') return '删除已请求/正在回收，完成后将无法打开控制台。'
  return '打开项目环境控制台'
}

function nextActionLabel(env: EnvironmentSummary): string {
  if (failedCleanup(env)) return '环境回收失败，资源释放尚未确认；请进入环境页重试回收'
  if (env.observedState === 'deleted') return '已删除；请创建新的项目环境'
  if (env.observedState === 'deleting' || env.desiredState === 'deleted') return '删除已请求/正在回收；请等待清理完成'
  if (hasActiveOperation(env)) return '正在处理；请等待当前操作完成'
  if (env.observedState === 'ready') return '已就绪；可以打开控制台'
  if (env.observedState === 'stopped') return '已停止；可以启动环境'
  if (env.observedState === 'failed') return env.currentOperation?.retryEligible ? '操作失败；可以重试失败操作' : '操作失败；请查看详情中的诊断'
  return `当前为${environmentStateLabel(env.observedState)}；请等待状态更新`
}

function openCreateDrawer() {
  showCreateModal.value = true
  void releases.load()
}

function resourceRequestLink(target?: { releaseId: string; releaseVersion: number }) {
  return {
    path: '/researcher/resources',
    query: {
      ...(projectId.value ? { projectId: projectId.value } : {}),
      ...(target ? { releaseId: target.releaseId, releaseVersion: String(target.releaseVersion) } : {}),
    },
  }
}

function requestReleaseTitle(request: EnvironmentRequest): string {
  const release = requestRelease(request)
  return release ? workTemplateTitle(release) : `实验版本 v${request.target.releaseVersion}`
}

/** The request carries no reviewer note, only an optional platform code. */
function rejectionReason(request: EnvironmentRequest): string {
  const reason = '管理员拒绝了这次资源申请，没有为它分配资源。如需了解原因，请联系课程教师或管理员；按实验说明调整资源后可以重新申请。'
  if (!request.diagnosticCode) return reason
  const platformReason = resourceAllocationFailureMessage(request.diagnosticCode)
  return platformReason
    ? `${reason}平台说明：${platformReason}（${request.diagnosticCode}）`
    : `${reason}平台诊断码：${request.diagnosticCode}。`
}

function reloadRequests() {
  void resources.load()
  void releases.load()
}

/**
 * "进入环境" attaches this browser to the environment Resource creates for the
 * approved request. One Idempotency-Key per (student, request) is kept as a
 * pending command, so a retry or a reload replays the same attach instead of
 * sending a new one. Only an accepted attach drops it, and only while it is
 * still the stored command: the server answers a completed attach before any
 * other check, so keeping the key after a refusal is always safe. An
 * allocation that is still running is not a failure; the student enters again
 * once it is ready.
 */
async function enterEnvironment(request: EnvironmentRequest) {
  const actor = actorId.value
  const targetProjectId = projectId.value
  if (!actor || !targetProjectId || request.projectId !== targetProjectId || attaching.value.has(request.id)) return
  const scope = `environment-attach:${request.id}`
  const fingerprint = [request.id, request.projectId, request.courseId ?? '', request.target.releaseId, request.target.releaseVersion].join(':')
  const stored = readPendingCommand<null>(actor, scope)
  const command = stored?.fingerprint === fingerprint ? stored : { fingerprint, idempotencyKey: idempotencyKey(), data: null }
  writePendingCommand(actor, scope, command)
  // A late answer for another project or student only updates storage, never this page.
  const current = () => projectId.value === targetProjectId && actorId.value === actor
  attaching.value.add(request.id)
  setAttachOutcome(request.id, null)
  try {
    const result = await lifecycle.create(
      {
        resourceRequestId: request.id,
        releaseId: request.target.releaseId,
        releaseVersion: request.target.releaseVersion,
        courseId: request.courseId ?? null,
      },
      command.idempotencyKey,
    )
    if (result.ok) {
      clearPendingCommand(actor, scope, command.idempotencyKey)
      if (!current()) return
      void router.push({
        path: props.environmentPath,
        query: { environmentId: result.accepted.environmentId, projectId: targetProjectId },
      })
      return
    }
    if (!current()) return
    setAttachOutcome(request.id, result.diagnostic.code === 'LW_ENVIRONMENT_ALLOCATION_PENDING'
      ? { kind: 'preparing' }
      : { kind: 'failed', diagnostic: result.diagnostic })
  } finally {
    attaching.value.delete(request.id)
  }
}

async function load() {
  const id = projectId.value
  const generation = ++loadGeneration
  if (!id) {
    state.value = isProjectContextLoading.value
      ? { kind: 'loading', message: '正在同步项目上下文…' }
      : {
          kind: 'blocked',
          diagnostic: makeDiagnostic('PROJECT_CONTEXT_MISSING', '项目上下文缺失，无法加载实验列表。', false),
        }
    return
  }
  state.value = { kind: 'loading', message: '加载实验列表…' }
  let items: EnvironmentSummary[]
  try {
    items = await fetchProjectEnvironments({
      projectId: id,
      ...(!props.teacherMode && courseId.value ? { courseId: courseId.value } : {}),
    })
  } catch (error) {
    if (generation !== loadGeneration || projectId.value !== id) return
    const problem = extractProblemDetails(error)
    state.value = {
      kind: 'error',
      diagnostic: makeDiagnostic(
        problem?.diagnosticCode ?? 'ENVIRONMENT_LIST_FAILED',
        problem?.detail ?? '加载实验列表失败',
        problem?.retryable ?? true,
        problem,
      ),
    }
    return
  }
  if (generation !== loadGeneration || projectId.value !== id) return
  state.value = items.length > 0 ? { kind: 'success', data: items } : { kind: 'empty' }
  scheduleRefresh()
}

/**
 * Non-terminal rows refresh every 5s if auto-refresh is enabled.
 */
function scheduleRefresh() {
  if (pollTimer) {
    clearTimeout(pollTimer)
    pollTimer = null
  }
  if (!autoRefreshEnabled.value || state.value.kind !== 'success') return
  const transitioning = state.value.data.some((row) =>
    !['ready', 'stopped', 'failed', 'deleted', 'deleting', 'expiring'].includes(row.observedState),
  )
  if (!transitioning || document.visibilityState === 'hidden') return
  pollTimer = setTimeout(() => {
    pollTimer = null
    void load()
  }, 5000)
}

function onVisibilityChange() {
  if (document.visibilityState === 'visible' && autoRefreshEnabled.value) void load()
}

watch(
  [() => projects.projects, routeProjectId],
  ([state, requestedProjectId]) => {
    if (state.kind !== 'success' || !requestedProjectId) return
    if (!state.data.some((project) => project.id === requestedProjectId)) return
    if (projects.selectedProjectId !== requestedProjectId) projects.select(requestedProjectId)
  },
  { immediate: true },
)

watch(
  () => projects.selectedProjectId,
  (projectId) => {
    if (!projectId || routeProjectInvalid.value || route.query.projectId === projectId) return
    void router.replace({ query: { ...route.query, projectId } })
  },
)

watch(projectId, () => {
  attachOutcomes.value = {}
})

watch(projectId, load, { immediate: true })
watch(autoRefreshEnabled, (enabled) => {
  if (enabled) scheduleRefresh()
  else if (pollTimer) {
    clearTimeout(pollTimer)
    pollTimer = null
  }
})

if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', onVisibilityChange)
  onScopeDispose(() => {
    document.removeEventListener('visibilitychange', onVisibilityChange)
    if (pollTimer) clearTimeout(pollTimer)
  })
}

function openEnvironment(environmentId: string) {
  const environment = state.value.kind === 'success'
    ? state.value.data.find((item) => item.id === environmentId)
    : undefined
  if (environment && !canOpenConsole(environment)) {
    inspectedEnv.value = environment
    return
  }
  void router.push({
    path: props.environmentPath,
    query: {
      environmentId,
      ...(projectId.value ? { projectId: projectId.value } : {}),
    },
  })
}
</script>

<style scoped>
.my-labs {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}

.header-title-row {
  display: flex;
  align-items: center;
  gap: var(--space-3);
}

.header-badge {
  display: inline-flex;
  align-items: center;
  padding: 2px var(--space-2);
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-surface-container-high);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-small);
}

.project-context-loading {
  padding: var(--space-5);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
}

.env-name-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.env-name {
  font-weight: 500;
  color: var(--md-sys-color-on-surface);
}

.env-id-sub {
  font-family: monospace;
  font-size: 11px;
  color: var(--md-sys-color-on-surface-variant);
}

.env-kind-sub,
.env-next-action,
.inspect-next-action {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-small);
}

.inspect-next-action {
  margin: 0;
  padding: var(--space-3) var(--space-3);
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container-low);
}

.state-cell {
  display: grid;
  gap: var(--space-1);
}

.runtime-tag {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface);
}

.row-actions {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}

.labs-hint {
  margin: var(--space-2) 0 0;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.request-section {
  display: grid;
  gap: var(--space-3);
  padding: var(--space-4) var(--space-5);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
}

.request-section__head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-2);
}

.request-section__head h3 {
  margin: 0;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-medium);
}

.request-section__subtitle {
  margin: var(--space-1) 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-large);
}

.request-section__hint {
  margin: 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.request-list {
  display: grid;
  gap: var(--space-2);
  margin: 0;
  padding: 0;
  list-style: none;
}

.request-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: center;
  gap: var(--space-2) var(--space-4);
  padding: var(--space-3) 0;
  border-top: 1px solid var(--md-sys-color-outline-variant);
}

.request-row__main {
  display: grid;
  gap: 2px;
  min-width: 0;
}

.request-row__main strong {
  overflow-wrap: anywhere;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-small);
}

.request-row__meta {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.request-row__meta--error {
  color: var(--md-sys-color-error);
}

.request-row__diagnostic {
  grid-column: 1 / -1;
}

.empty-labs-pane {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: var(--space-3);
  padding: var(--space-16) var(--space-6);
  text-align: center;
  color: var(--md-sys-color-on-surface-variant);
}

.empty-labs-pane h3 {
  font: var(--md-sys-title-medium);
  color: var(--md-sys-color-on-surface);
}

.empty-labs-pane p {
  max-width: 420px;
  font: var(--md-sys-body-medium);
}

/* Side Sheet Inspector */
.inspect-content {
  display: flex;
  flex-direction: column;
  gap: var(--space-5);
}

.inspect-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
  padding-bottom: var(--space-3);
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.inspect-properties {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}

.prop-row {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
}

.prop-label {
  font: var(--md-sys-label-small);
  color: var(--md-sys-color-on-surface-variant);
}

.prop-value {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface);
}

.prop-value-with-copy {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}

.prop-value-with-copy code {
  font-family: monospace;
  font-size: 12px;
  background: var(--md-sys-color-surface-container);
  padding: 2px var(--space-2);
  border-radius: 4px;
}

.inspect-actions {
  margin-top: var(--space-4);
}

.full-width {
  width: 100%;
}

/* Create Modal */
.create-modal-overlay {
  position: fixed;
  inset: 0;
  z-index: 1400;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--md-sys-color-scrim);
  padding: var(--space-4);
}

.create-modal {
  width: 100%;
  max-width: 680px;
  max-height: 85vh;
  display: flex;
  flex-direction: column;
  background: var(--md-sys-color-surface);
  border-radius: var(--md-sys-shape-large);
  box-shadow: var(--md-sys-elevation-3);
  overflow: hidden;
}

.modal-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--space-4) var(--space-5);
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.modal-title-group {
  display: flex;
  align-items: center;
  gap: var(--space-3);
}

.modal-icon {
  color: var(--md-sys-color-primary);
}

.modal-body {
  padding: var(--space-5);
  overflow-y: auto;
}

.modal-subtitle {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
  margin-bottom: var(--space-4);
}

.release-cards-grid {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}

.release-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--space-4) var(--space-4);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container);
  gap: var(--space-3);
  flex-wrap: wrap;
}

.release-card__top {
  display: flex;
  align-items: center;
  gap: var(--space-3);
}

.release-runtime-badge {
  display: inline-flex;
  align-items: center;
  gap: var(--space-1);
  font: var(--md-sys-label-small);
  padding: 2px var(--space-2);
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-surface-container-high);
}

.release-id {
  font-family: monospace;
  font-size: 12px;
}

.release-title {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-small);
}

.release-meta {
  display: flex;
  flex-direction: column;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  font-size: 11px;
}

.empty-release-note {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-8) var(--space-4);
  color: var(--md-sys-color-on-surface-variant);
  text-align: center;
}
</style>
