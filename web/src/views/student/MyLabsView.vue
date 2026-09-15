<template>
  <div class="my-labs">
    <header class="page-header">
      <div class="header-title-row">
        <h2>我的项目环境</h2>
        <span class="header-badge" v-if="state.kind === 'success'">{{ state.data.length }} 个环境</span>
      </div>
      <p class="page-subtitle">查看当前项目创建的环境；进入终端控制台或提交评测材料。</p>
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
      message="项目上下文未绑定，无法加载你的实验列表。请通过顶栏项目选择器选择项目或联系管理员。"
      :retryable="false"
      severity="error"
    />

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
            type="button"
            class="filled-button"
            @click="openCreateDrawer"
          >
            <SvgIcon name="add" size="sm" aria-hidden="true" />
            <span>创建项目环境</span>
          </button>
        </template>
      </GcpActionBar>

      <DiagnosticBanner
        v-if="createDiagnostic"
        :code="createDiagnostic.code"
        :message="createDiagnostic.message"
        :retryable="createDiagnostic.retryable"
        severity="error"
        @retry="retryCreate"
      />

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
            aria-label="我的项目环境列表"
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
              {{ formatTimestamp(row.eligibilityExpiresAt) }}
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
            点击行可原地查看项目环境规格与端点详情；点击「创建项目环境」从已发布模板快速启动新环境。
          </p>
        </template>

        <template #empty>
          <div class="empty-labs-pane">
            <SvgIcon name="science" size="xl" aria-hidden="true" />
            <h3>暂无项目环境</h3>
            <p>当前项目还没有环境。可从教师已发布的模板创建一个项目环境。</p>
            <button
              type="button"
              class="filled-button"
              @click="openCreateDrawer"
            >
              <SvgIcon name="add" size="sm" aria-hidden="true" />
              <span>创建第一个项目环境</span>
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
              <span class="prop-label">到期时间</span>
              <span class="prop-value">{{ formatTimestamp(inspectedEnv.eligibilityExpiresAt) }}</span>
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
        </div>
      </div>
    </EvidenceSideSheet>

    <!-- Create Environment Modal (GCP Style Template Selector) -->
    <div
      v-if="showCreateModal"
      class="create-modal-overlay"
      role="dialog"
      aria-label="创建项目环境"
      @click.self="showCreateModal = false"
    >
      <div class="create-modal md-card">
        <div class="modal-header">
          <div class="modal-title-group">
            <SvgIcon name="add_circle" size="md" class="modal-icon" aria-hidden="true" />
            <h3>从已发布版本创建项目环境</h3>
          </div>
          <button
            type="button"
            class="icon-button"
            aria-label="关闭创建弹窗"
            @click="showCreateModal = false"
          >
            <SvgIcon name="close" size="md" aria-hidden="true" />
          </button>
        </div>

        <div class="modal-body">
          <AsyncStateView :state="releases.releases" @retry="releases.load">
            <template #success="{ data: releaseList }">
              <p class="modal-subtitle">选择教师已发布并经过门禁验证的项目环境模板：</p>
              <div class="release-cards-grid">
                <div
                  v-for="rel in releaseList"
                  :key="rel.id"
                  class="release-card"
                  :class="{ 'release-card--creating': lifecycle.operating.has(`create:${rel.id}`) }"
                >
                  <div class="release-card__top">
                    <span class="release-runtime-badge" :class="`badge--${rel.runtimeKind}`">
                      <SvgIcon :name="rel.runtimeKind === 'container' ? 'inventory_2' : 'dns'" size="sm" aria-hidden="true" />
                      {{ rel.runtimeKind === 'container' ? '容器' : '虚拟机' }}
                    </span>
                    <code class="release-id">{{ rel.id }}</code>
                  </div>
                  <div class="release-meta">
                    <span>发布者：{{ rel.publishedBy }}</span>
                    <span>发布时间：{{ formatTimestamp(rel.publishedAt) }}</span>
                  </div>
                  <button
                    type="button"
                    class="filled-button small"
                    :disabled="lifecycle.operating.has(`create:${rel.id}`)"
                    @click="handleCreate(rel)"
                  >
                    {{ lifecycle.operating.has(`create:${rel.id}`) ? '正在置备…' : '选择并创建' }}
                  </button>
                </div>
              </div>
            </template>
            <template #empty>
              <div class="empty-release-note">
                <SvgIcon name="info" size="lg" aria-hidden="true" />
                <p>当前课程暂无已发布的环境版本模板，请联系课程教师在教师工作台中发布模板。</p>
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
import { useRoute, useRouter } from 'vue-router'
import { useEnvironmentTemplateReleases } from '@/composables/useEnvironmentTemplateReleases'
import { useEnvironmentLifecycle } from '@/composables/useEnvironmentLifecycle'
import { useProjects } from '@/composables/useProjects'
import { listEnvironments } from '@/generated/contracts'
import type { EnvironmentSummary, EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'
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
import { environmentStateLabel } from '@/utils/stateLabels'

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
const isContextMissing = computed(() => !projectId.value)
const isInvalidProjectContext = computed(() => routeProjectInvalid.value)

const router = useRouter()
const releases = useEnvironmentTemplateReleases(projectId, courseId)
const lifecycle = useEnvironmentLifecycle(projectId, courseId)

const state = ref<AsyncState<EnvironmentSummary[]>>({ kind: 'idle' })
const autoRefreshEnabled = ref(true)
let pollTimer: ReturnType<typeof setTimeout> | null = null

const inspectedEnv = ref<EnvironmentSummary | null>(null)
const showCreateModal = ref(false)
const createDiagnostic = ref<DiagnosticViewModel | null>(null)
const pendingRelease = ref<EnvironmentTemplateReleaseViewSchema | null>(null)

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
  { key: 'eligibilityExpiresAt', title: '到期时间' },
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

function hasActiveOperation(env: EnvironmentSummary): boolean {
  const operation = env.currentOperation
  return Boolean(operation && ['accepted', 'running', 'cancelling'].includes(operation.state))
}

function canOpenConsole(env: EnvironmentSummary): boolean {
  return env.desiredState !== 'deleted' && env.observedState !== 'deleting' && env.observedState !== 'deleted'
}

function consoleActionReason(env: EnvironmentSummary): string {
  if (env.observedState === 'deleted') return '此项目环境已删除，无法打开控制台。请创建新的项目环境。'
  if (env.observedState === 'deleting' || env.desiredState === 'deleted') return '删除已请求/正在回收，完成后将无法打开控制台。'
  return '打开项目环境控制台'
}

function nextActionLabel(env: EnvironmentSummary): string {
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
  createDiagnostic.value = null
  void releases.load()
}

async function handleCreate(rel: EnvironmentTemplateReleaseViewSchema) {
  createDiagnostic.value = null
  pendingRelease.value = rel
  const result = await lifecycle.create(
    {
      releaseId: rel.id,
      releaseVersion: rel.version,
    },
    idempotencyKey(),
  )
  if (result.ok) {
    pendingRelease.value = null
    showCreateModal.value = false
    await load()
  } else {
    createDiagnostic.value = result.diagnostic
  }
}

function retryCreate() {
  if (pendingRelease.value) void handleCreate(pendingRelease.value)
}

async function load() {
  const id = projectId.value
  if (!id) {
    state.value = {
      kind: 'blocked',
      diagnostic: makeDiagnostic('PROJECT_CONTEXT_MISSING', '项目上下文缺失，无法加载实验列表。', false),
    }
    return
  }
  state.value = { kind: 'loading', message: '加载实验列表…' }
  const result = await listEnvironments({
    query: {
      projectId: id,
      ...(courseId.value ? { courseId: courseId.value } : {}),
    },
  })
  if (result.error) {
    const problem = extractProblemDetails(result.error)
    state.value = {
      kind: 'error',
      diagnostic: makeDiagnostic(
        problem?.diagnosticCode ?? 'ENVIRONMENT_LIST_FAILED',
        problem?.detail ?? '加载实验列表失败',
        problem?.retryable ?? true,
      ),
    }
    return
  }
  const items = result.data.items ?? []
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
    path: '/student/environments',
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
  gap: 16px;
}

.header-title-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.header-badge {
  display: inline-flex;
  align-items: center;
  padding: 2px 8px;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-surface-container-high);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-small);
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
  padding: 10px 12px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container-low);
}

.state-cell {
  display: grid;
  gap: 4px;
}

.runtime-tag {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface);
}

.row-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.filled-button,
.outlined-button,
.text-button {
  min-height: 40px;
  padding: 0 17px;
  border-radius: var(--md-sys-shape-full);
  font: var(--md-sys-label-large);
  cursor: pointer;
  text-decoration: none;
}

.filled-button.small,
.outlined-button.small {
  min-height: 32px;
  padding: 0 12px;
  font: var(--md-sys-label-medium);
}

.filled-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  border: 1px solid var(--md-sys-color-primary);
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
}

.outlined-button {
  border: 1px solid var(--md-sys-color-outline);
  background: transparent;
  color: var(--md-sys-color-primary);
}

.text-button {
  min-height: 32px;
  border: 0;
  background: transparent;
  color: var(--md-sys-color-primary);
}

.filled-button:disabled,
.outlined-button:disabled,
.text-button:disabled {
  opacity: .5;
  cursor: not-allowed;
}

.labs-hint {
  margin: 8px 0 0;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.empty-labs-pane {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  padding: 64px 24px;
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
  gap: 20px;
}

.inspect-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.inspect-properties {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.prop-row {
  display: flex;
  flex-direction: column;
  gap: 4px;
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
  gap: 8px;
}

.prop-value-with-copy code {
  font-family: monospace;
  font-size: 12px;
  background: var(--md-sys-color-surface-container);
  padding: 2px 6px;
  border-radius: 4px;
}

.inspect-actions {
  margin-top: 16px;
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
  padding: 16px;
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
  padding: 16px 20px;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.modal-title-group {
  display: flex;
  align-items: center;
  gap: 10px;
}

.modal-icon {
  color: var(--md-sys-color-primary);
}

.modal-body {
  padding: 20px;
  overflow-y: auto;
}

.modal-subtitle {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
  margin-bottom: 16px;
}

.release-cards-grid {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.release-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container);
  gap: 12px;
  flex-wrap: wrap;
}

.release-card__top {
  display: flex;
  align-items: center;
  gap: 10px;
}

.release-runtime-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font: var(--md-sys-label-small);
  padding: 2px 8px;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-surface-container-high);
}

.release-id {
  font-family: monospace;
  font-size: 12px;
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
  gap: 10px;
  padding: 32px 16px;
  color: var(--md-sys-color-on-surface-variant);
  text-align: center;
}
</style>
