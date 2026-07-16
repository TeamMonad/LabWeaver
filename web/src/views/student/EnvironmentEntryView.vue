<template>
  <div class="environment-entry">
    <header class="page-header">
      <h2>环境控制台</h2>
      <p class="page-subtitle">选择已发布版本创建环境，管理生命周期并获取 SSH/HTTP 访问授权。</p>
    </header>

    <DiagnosticBanner
      v-if="isContextMissing"
      code="COURSE_CONTEXT_MISSING"
      message="课程上下文未绑定，无法加载环境模板版本。请通过课程选择器选择课程或联系管理员完成 #47。"
      :retryable="false"
      severity="error"
    />
    <DiagnosticBanner
      v-else-if="isContextFromEnv"
      code="COURSE_CONTEXT_FROM_ENV"
      message="当前使用部署配置中的默认课程上下文；真实课程选择待 #47 接入。"
      :retryable="false"
      severity="warning"
    />

    <section aria-labelledby="releases-heading">
      <h3 id="releases-heading" class="section-title">
        <SvgIcon name="deployed_code" size="sm" aria-hidden="true" />
        已发布版本
      </h3>
      <AsyncStateView :state="releases.releases" @retry="releases.load">
        <template #success="{ data }">
          <DataTable :columns="releaseColumns" :rows="data" aria-label="已发布环境版本">
            <template #runtimeKind="{ row }">
              {{ row.runtimeKind === 'container' ? '容器' : '虚拟机' }}
            </template>
            <template #publishedAt="{ row }">
              {{ formatTimestamp(row.publishedAt) }}
            </template>
            <template #actions="{ row }">
              <button
                type="button"
                class="filled-button small"
                :disabled="lifecycle.operating.has(`create:${row.id}`)"
                @click="createFromRelease(row)"
              >
                创建环境
              </button>
            </template>
          </DataTable>
        </template>
      </AsyncStateView>

      <div v-if="createDiagnostic" class="create-result">
        <DiagnosticBanner
          :code="createDiagnostic.code"
          :message="createDiagnostic.message"
          :retryable="createDiagnostic.retryable"
          severity="info"
          @retry="retryCreate"
        />
      </div>
      <div v-else-if="lifecycle.lastAccepted" class="create-result create-result--ok">
        <SvgIcon name="check_circle" size="md" aria-hidden="true" />
        <span>
          创建请求已接受：{{ lifecycle.lastAccepted.operationId }}
          <span v-if="!selectedEnvironmentId" class="create-hint">
            （正在加载新建环境…）
          </span>
        </span>
      </div>
    </section>

    <section class="console-section" aria-labelledby="console-heading">
      <h3 id="console-heading" class="section-title">
        <SvgIcon name="desktop_windows" size="sm" aria-hidden="true" />
        环境控制台
      </h3>

      <div class="environment-selector">
        <label for="env-id-input">环境 ID</label>
        <input
          id="env-id-input"
          v-model="environmentIdInput"
          type="text"
          class="text-input"
          placeholder="输入环境 ID 或从创建请求获取"
        />
        <button type="button" class="filled-button" @click="applyEnvironmentId">加载</button>
      </div>

      <div v-if="!selectedEnvironmentId" class="placeholder-pane">
        <SvgIcon name="info" size="lg" aria-hidden="true" />
        <p>选择版本创建环境，或输入已有环境 ID 开始管理。</p>
      </div>

      <template v-else>
        <AsyncStateView :state="env.instance" @retry="env.load">
          <template #success="{ data }">
            <div class="env-card md-card">
              <div class="env-header">
                <div>
                  <span class="env-id">{{ data.id }}</span>
                  <span class="env-runtime">{{ data.runtimeKind === 'container' ? '容器' : '虚拟机' }}</span>
                </div>
                <span class="env-state" :class="`env-state--${data.observedState}`">{{ data.observedState }}</span>
              </div>

              <div class="env-meta">
                <span>期望状态：{{ data.desiredState }}</span>
                <span>版本：rev-{{ data.revision }}</span>
                <span>过期时间：{{ formatTimestamp(data.eligibilityExpiresAt) }}</span>
              </div>

              <div class="env-actions">
                <button
                  type="button"
                  class="filled-button"
                  :disabled="!canStart(data)"
                  @click="lifecycle.act(data.id, data.revision, 'start')"
                >
                  启动
                </button>
                <button
                  type="button"
                  class="outlined-button"
                  :disabled="!canStop(data)"
                  @click="lifecycle.act(data.id, data.revision, 'stop')"
                >
                  停止
                </button>
                <button
                  type="button"
                  class="outlined-button"
                  :disabled="!canRestart(data)"
                  @click="lifecycle.act(data.id, data.revision, 'restart')"
                >
                  重启
                </button>
                <button
                  type="button"
                  class="text-button error"
                  :disabled="!canDelete(data)"
                  @click="openDelete(data)"
                >
                  删除
                </button>
              </div>
            </div>

            <div class="access-section">
              <h4 class="section-subtitle">访问授权</h4>
              <AsyncStateView :state="access.endpoints" @retry="access.loadEndpoints">
                <template #success="{ data: eps }">
                  <DataTable :columns="endpointColumns" :rows="eps" aria-label="环境入口">
                    <template #protocol="{ row }">
                      <span class="tag">{{ row.protocol }}</span>
                    </template>
                    <template #health="{ row }">
                      <span class="health-dot" :class="`health-dot--${row.health}`" />
                      {{ row.health }}
                    </template>
                    <template #observedAt="{ row }">
                      {{ formatTimestamp(row.observedAt) }}
                    </template>
                  </DataTable>

                  <div class="grant-actions">
                    <button
                      v-if="access.grant.kind !== 'success'"
                      type="button"
                      class="filled-button"
                      :disabled="access.creating || eps.length === 0"
                      @click="access.createGrant"
                    >
                      签发访问授权
                    </button>
                    <button
                      v-if="access.grant.kind === 'success'"
                      type="button"
                      class="text-button error"
                      @click="access.revokeGrant"
                    >
                      撤销授权
                    </button>
                  </div>

                  <AsyncStateView :state="access.grant" @retry="access.createGrant">
                    <template #success="{ data: g }">
                      <div class="grant-card">
                        <div class="grant-row">
                          <span>授权 ID</span>
                          <code>{{ g.id }}</code>
                        </div>
                        <div class="grant-row">
                          <span>状态</span>
                          <span class="env-state" :class="`env-state--${g.state}`">{{ g.state }}</span>
                        </div>
                        <div class="grant-row">
                          <span>有效期</span>
                          <span>{{ formatTimestamp(g.issuedAt) }} → {{ formatTimestamp(g.expiresAt) }}</span>
                        </div>
                        <div class="grant-row">
                          <span>入口授权</span>
                          <div class="endpoint-grants">
                            <span v-for="eg in g.endpointGrants" :key="eg.id" class="tag">
                              {{ eg.protocol }} {{ eg.alias ?? eg.endpointId }}
                            </span>
                          </div>
                        </div>
                      </div>
                    </template>
                  </AsyncStateView>
                </template>
              </AsyncStateView>
            </div>

            <div v-if="timelineEvents.length > 0" class="timeline-section">
              <h4 class="section-subtitle">生命周期事件</h4>
              <EventTimeline :events="timelineEvents" aria-label="环境生命周期时间线" />
            </div>
          </template>
        </AsyncStateView>
      </template>
    </section>

    <ConfirmDialog
      :open="deleteEnvironment !== null"
      title="删除环境"
      description="确定删除该环境吗？所有未持久化的数据将丢失。"
      confirm-text="删除"
      severity="error"
      @cancel="deleteEnvironment = null"
      @confirm="confirmDeleteEnvironment"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useCourseContext } from '@/composables/useCourseContext'
import { useEnvironmentTemplateReleases } from '@/composables/useEnvironmentTemplateReleases'
import { useEnvironmentInstance } from '@/composables/useEnvironmentInstance'
import { useEnvironmentLifecycle } from '@/composables/useEnvironmentLifecycle'
import { useEnvironmentAccess } from '@/composables/useEnvironmentAccess'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DataTable from '@/components/common/DataTable.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import EventTimeline from '@/components/common/EventTimeline.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { formatTimestamp } from '@/utils/format'
import { makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import type { DataTableColumn } from '@/components/common/DataTable.vue'
import type { EnvironmentInstanceSchema, EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'
import type { TimelineEvent } from '@/components/common/EventTimeline.vue'

const course = useCourseContext()
const courseId = course.courseId
const isContextMissing = computed(() => course.context.value === null)
const isContextFromEnv = computed(() => course.context.value?.source === 'env')

const route = useRoute()
const router = useRouter()

const releases = useEnvironmentTemplateReleases(courseId)
const lifecycle = useEnvironmentLifecycle(courseId)

const selectedEnvironmentId = ref<string | undefined>(
  typeof route.query.environmentId === 'string' ? route.query.environmentId : undefined,
)
const environmentIdInput = ref(selectedEnvironmentId.value ?? '')
const createDiagnostic = ref<DiagnosticViewModel | null>(null)
const pendingRelease = ref<EnvironmentTemplateReleaseViewSchema | null>(null)
const deleteEnvironment = ref<EnvironmentInstanceSchema | null>(null)

const env = useEnvironmentInstance(selectedEnvironmentId)
const access = useEnvironmentAccess(
  selectedEnvironmentId,
  computed(() => (env.instance.kind === 'success' ? env.instance.data.revision : undefined)),
  courseId,
)

watch(
  () => route.query.environmentId,
  (q) => {
    selectedEnvironmentId.value = typeof q === 'string' ? q : undefined
    environmentIdInput.value = selectedEnvironmentId.value ?? ''
  },
)

watch(selectedEnvironmentId, (id) => {
  access.resetGrant()
  if (id) access.loadEndpoints()
})

const releaseColumns: DataTableColumn<EnvironmentTemplateReleaseViewSchema & { actions?: never }>[] = [
  { key: 'runtimeKind', title: 'Runtime' },
  { key: 'id', title: '版本 ID' },
  { key: 'publishedBy', title: '发布者' },
  { key: 'publishedAt', title: '发布时间' },
  { key: 'actions', title: '操作' },
]

const endpointColumns: DataTableColumn<{ protocol: string; health: string; observedAt: string; id: string }>[] = [
  { key: 'protocol', title: '协议' },
  { key: 'health', title: '健康' },
  { key: 'observedAt', title: '观测时间' },
]

const timelineEvents = computed<TimelineEvent[]>(() => {
  if (env.instance.kind !== 'success') return []
  const op = env.instance.data.operation
  return [
    {
      id: `${op.kind}-${op.acceptedAt}`,
      title: op.kind,
      timestamp: op.acceptedAt,
      description: `actor: ${op.actorId}, revision: ${op.acceptedRevision}`,
    },
  ]
})

function canStart(data: EnvironmentInstanceSchema) {
  return data.desiredState !== 'running' && !lifecycle.operating.has(`${data.id}:start`)
}
function canStop(data: EnvironmentInstanceSchema) {
  return data.desiredState !== 'stopped' && !lifecycle.operating.has(`${data.id}:stop`)
}
function canRestart(data: EnvironmentInstanceSchema) {
  return !lifecycle.operating.has(`${data.id}:restart`)
}
function canDelete(data: EnvironmentInstanceSchema) {
  return !lifecycle.operating.has(`${data.id}:delete`)
}

function applyEnvironmentId() {
  const id = environmentIdInput.value.trim()
  if (!id) return
  router.replace({ query: { ...route.query, environmentId: id } })
}

async function createFromRelease(release: EnvironmentTemplateReleaseViewSchema) {
  createDiagnostic.value = null
  pendingRelease.value = release
  lifecycle.operating.add(`create:${release.id}`)
  try {
    const result = await lifecycle.create({
      courseId: courseId.value ?? '',
      releaseId: release.id,
      releaseVersion: release.releaseVersion,
    })
    if (!result.ok) {
      createDiagnostic.value = result.diagnostic
    } else if (result.accepted?.environmentId) {
      router.replace({ query: { ...route.query, environmentId: result.accepted.environmentId } })
    }
  } finally {
    lifecycle.operating.delete(`create:${release.id}`)
  }
}

function retryCreate() {
  if (pendingRelease.value) createFromRelease(pendingRelease.value)
}

function openDelete(data: EnvironmentInstanceSchema) {
  deleteEnvironment.value = data
}

async function confirmDeleteEnvironment() {
  if (!deleteEnvironment.value) return
  const data = deleteEnvironment.value
  deleteEnvironment.value = null
  const result = await lifecycle.act(data.id, data.revision, 'delete')
  if (!result.ok) {
    createDiagnostic.value = result.diagnostic
  }
}
</script>

<style scoped>
.environment-entry {
  display: flex;
  flex-direction: column;
  gap: 28px;
}

.page-header h2 {
  font: var(--md-sys-headline-small);
  color: var(--md-sys-color-on-surface);
  margin: 0;
}

.page-subtitle {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
  margin: 4px 0 0;
}

.section-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font: var(--md-sys-title-medium);
  color: var(--md-sys-color-on-surface);
  margin: 0 0 12px;
}

.section-subtitle {
  font: var(--md-sys-title-small);
  color: var(--md-sys-color-on-surface);
  margin: 16px 0 8px;
}

.create-result {
  margin-top: 12px;
  padding: 10px 12px;
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-tertiary-container);
  color: var(--md-sys-color-on-tertiary-container);
  font: var(--md-sys-body-medium);
}

.create-result--ok {
  display: flex;
  align-items: center;
  gap: 8px;
}

.create-hint {
  opacity: 0.8;
}

.environment-selector {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 16px;
}

.environment-selector label {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.text-input {
  flex: 1;
  min-width: 0;
  height: 40px;
  padding: 0 12px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
}

.text-input:focus-visible {
  outline: 2px solid var(--md-sys-color-primary);
  outline-offset: 2px;
}

.placeholder-pane {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 48px 24px;
  text-align: center;
  color: var(--md-sys-color-on-surface-variant);
}

.env-card,
.grant-card {
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.env-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 12px;
}

.env-id {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
  word-break: break-all;
}

.env-runtime {
  margin-left: 12px;
  padding: 2px 8px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
  font: var(--md-sys-label-medium);
}

.env-state {
  padding: 4px 12px;
  border-radius: var(--md-sys-shape-small);
  font: var(--md-sys-label-large);
  text-transform: capitalize;
}

.env-state--requested,
.env-state--validating,
.env-state--building,
.env-state--provisioning,
.env-state--stopping,
.env-state--updating,
.env-state--expiring,
.env-state--deleting {
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
}
.env-state--ready {
  background: var(--md-sys-color-tertiary-container);
  color: var(--md-sys-color-on-tertiary-container);
}
.env-state--stopped {
  background: var(--md-sys-color-surface-container-highest);
  color: var(--md-sys-color-on-surface-variant);
}
.env-state--failed,
.env-state--deleted {
  background: var(--md-sys-color-error-container);
  color: var(--md-sys-color-on-error-container);
}

.env-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 16px;
  margin-bottom: 16px;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.env-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
}

.filled-button {
  height: 40px;
  padding: 0 24px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.filled-button.small {
  height: 32px;
  padding: 0 16px;
}

.filled-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.outlined-button {
  height: 40px;
  padding: 0 24px;
  border: 1px solid var(--md-sys-color-outline);
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.outlined-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.text-button {
  height: 40px;
  padding: 0 16px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.text-button.error {
  color: var(--md-sys-color-error);
}

.text-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.access-section {
  margin-top: 24px;
}

.grant-actions {
  display: flex;
  gap: 12px;
  margin-top: 16px;
}

.grant-row {
  display: flex;
  gap: 16px;
  padding: 8px 0;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.grant-row:last-child {
  border-bottom: none;
}

.grant-row > span:first-child {
  width: 100px;
  flex-shrink: 0;
  color: var(--md-sys-color-on-surface-variant);
}

.endpoint-grants {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.tag {
  padding: 4px 10px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
  font: var(--md-sys-label-medium);
}

.health-dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  margin-right: 6px;
  border-radius: 50%;
  background: var(--md-sys-color-outline);
}

.health-dot--healthy { background: var(--md-sys-color-tertiary); }
.health-dot--unhealthy { background: var(--md-sys-color-error); }
.health-dot--pending { background: var(--md-sys-color-primary); }
.health-dot--removed { background: var(--md-sys-color-outline); }

.timeline-section {
  margin-top: 24px;
}
</style>
