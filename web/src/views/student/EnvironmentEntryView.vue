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
      <AsyncStateView v-if="!isContextMissing" :state="releases.releases" @retry="releases.load">
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
                <span class="env-state" :class="`env-state--${data.observedState}`">{{ environmentStateLabel(data.observedState) }}</span>
              </div>

              <div class="env-meta">
                <span>期望状态：{{ environmentStateLabel(data.desiredState) }}</span>
                <span>修订：rev-{{ data.revision }}</span>
                <span>过期时间：{{ formatTimestamp(data.eligibilityExpiresAt) }}</span>
              </div>

              <div v-if="showProgression" class="env-progression" role="status">
                <ol class="progression-steps">
                  <li
                    v-for="(step, index) in PROGRESSION_STEPS"
                    :key="step"
                    class="progression-step"
                    :class="{
                      'progression-step--done': progressionStepIndex !== null && index < progressionStepIndex,
                      'progression-step--active': index === progressionStepIndex,
                    }"
                  >
                    {{ step }}
                  </li>
                </ol>
                <p v-if="activeOperation" class="progression-meta">
                  尝试 {{ activeOperation.attempt }}/{{ activeOperation.maxAttempts }}
                  <template v-if="activeOperation.providerPhase"> · 阶段：{{ activeOperation.providerPhase }}</template>
                  · 截止 {{ formatTimestamp(activeOperation.deadlineAt) }}
                </p>
              </div>

              <div v-if="data.observedState === 'failed'" class="env-failed-panel">
                <DiagnosticBanner
                  code="ENVIRONMENT_FAILED"
                  :message="`环境进入失败状态${data.failedPhase ? `（阶段：${environmentStateLabel(data.failedPhase)}）` : ''}${data.lastDiagnosticCode ? `，诊断码：${data.lastDiagnosticCode}` : ''}。可尝试重试失败的操作；重试为幂等操作，不会重复创建资源。`"
                  :retryable="true"
                  severity="error"
                />
                <div class="env-failed-actions">
                  <button
                    type="button"
                    class="outlined-button"
                    :disabled="retryingEnvironment"
                    @click="retryFailedOperation(data)"
                  >
                    {{ retryingEnvironment ? '重试中…' : '重试失败的操作' }}
                  </button>
                </div>
              </div>

              <div v-if="retryDiagnostic" class="lifecycle-result">
                <DiagnosticBanner
                  :code="retryDiagnostic.code"
                  :message="retryDiagnostic.message"
                  :retryable="retryDiagnostic.retryable"
                  severity="error"
                  @retry="retryFailedOperation(data)"
                />
              </div>

              <div class="env-actions">
                <button
                  type="button"
                  class="filled-button"
                  :disabled="!canStart(data)"
                  @click="runLifecycle(data, 'start')"
                >
                  启动
                </button>
                <button
                  type="button"
                  class="outlined-button"
                  :disabled="!canStop(data)"
                  @click="runLifecycle(data, 'stop')"
                >
                  停止
                </button>
                <button
                  type="button"
                  class="outlined-button"
                  :disabled="!canRestart(data)"
                  @click="runLifecycle(data, 'restart')"
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

              <div v-if="lifecycleDiagnostic" class="lifecycle-result">
                <DiagnosticBanner
                  :code="lifecycleDiagnostic.code"
                  :message="lifecycleDiagnostic.message"
                  :retryable="lifecycleDiagnostic.retryable"
                  severity="error"
                  @retry="retryLifecycle"
                />
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
                      {{ endpointHealthLabel(row.health) }}
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
                      @click="issueAccessGrant"
                    >
                      签发访问授权
                    </button>
                    <button
                      v-if="access.grant.kind === 'success'"
                      type="button"
                      class="text-button error"
                      @click="revokeAccessGrant"
                    >
                      撤销授权
                    </button>
                  </div>

                  <div v-if="createGrantDiagnostic" class="grant-result">
                    <DiagnosticBanner
                      :code="createGrantDiagnostic.code"
                      :message="createGrantDiagnostic.message"
                      :retryable="createGrantDiagnostic.retryable"
                      severity="error"
                      @retry="issueAccessGrant"
                    />
                  </div>

                  <AsyncStateView :state="access.grant" @retry="issueAccessGrant">
                    <template #success="{ data: g }">
                      <div class="grant-card">
                        <div class="grant-row">
                          <span>授权 ID</span>
                          <code>{{ g.id }}</code>
                        </div>
                        <div class="grant-row">
                          <span>状态</span>
                          <span class="env-state" :class="`env-state--${g.state}`">{{ accessGrantStateLabel(g.state) }}</span>
                        </div>
                        <div class="grant-row">
                          <span>有效期</span>
                          <span>{{ formatTimestamp(g.issuedAt) }} → {{ formatTimestamp(g.expiresAt) }}</span>
                        </div>
                        <div class="grant-row">
                          <span>过期</span>
                          <span>{{ formatExpiry(g.expiresAt) }}</span>
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

                      <div v-if="g.state === 'active'" class="runtime-access">
                        <div v-if="httpsGrant(g)" class="access-card">
                          <h5 class="access-card__title">
                            <SvgIcon name="code" size="sm" aria-hidden="true" />
                            容器实验入口
                          </h5>
                          <p class="access-card__desc">通过当前登录会话与 AccessGrant 打开受保护的容器实验页面。</p>
                          <button
                            type="button"
                            class="filled-button"
                            :disabled="!connectUrl(g)"
                            @click="openContainerRuntime(g)"
                          >
                            打开容器实验
                          </button>
                          <p v-if="!connectUrl(g)" class="access-card__hint">连接地址缺失，无法打开。</p>
                        </div>

                        <div v-if="sshGrant(g)" class="access-card">
                          <h5 class="access-card__title">
                            <SvgIcon name="terminal" size="sm" aria-hidden="true" />
                            SSH
                          </h5>
                          <p class="access-card__desc">单行命令到唯一 VM；无需下载配置。</p>
                          <div v-if="sshCommand(g)" class="ssh-command">
                            <code class="ssh-command__text">{{ sshCommand(g) }}</code>
                            <CopyButton :text="sshCommand(g) ?? ''" aria-label="复制 SSH 命令" />
                          </div>
                          <div v-if="sshCommand(g)" class="ssh-meta">
                            <span>Gateway fingerprint：<code>{{ sshFingerprint(g) ?? 'unavailable' }}</code></span>
                            <span>Grant：{{ formatExpiry(g.expiresAt) }}</span>
                          </div>
                          <p v-else class="access-card__hint">SSH 别名或 Gateway 缺失，无法生成命令。</p>
                        </div>
                      </div>

                      <div v-if="g.state === 'active' && data.observedState === 'ready'" class="console-section">
                        <ConsolePanel
                          v-if="data.runtimeKind === 'container'"
                          kind="xterm"
                          :grant="g"
                          :environment="data"
                        />
                        <ConsolePanel
                          v-else-if="data.runtimeKind === 'virtual_machine'"
                          kind="novnc"
                          :grant="g"
                          :environment="data"
                        />
                      </div>
                    </template>
                  </AsyncStateView>
                </template>
              </AsyncStateView>
            </div>

            <div class="freeze-section">
              <h4 class="section-subtitle">冻结提交与证据</h4>
              <p class="freeze-desc">将当前工作区冻结为不可变提交，保留 Collector object version 与 SHA-256。</p>
              <button
                type="button"
                class="filled-button"
                :disabled="!canFreeze(data) || freezeState.kind === 'loading'"
                @click="freezeConfirmVisible = true"
              >
                {{ freezeState.kind === 'loading' ? '冻结中…' : '冻结提交' }}
              </button>

              <div v-if="freezeConfirmVisible" class="freeze-confirm" role="dialog" aria-label="确认冻结清单">
                <h5 class="section-subtitle">确认冻结清单（SubmissionManifest）</h5>
                <pre class="freeze-manifest">{{ freezeManifestText }}</pre>
                <p class="freeze-confirm-hint">
                  提交前请确认清单覆盖全部必交文件；当前清单为课程默认工作区冻结规则，按提交规范定制清单的能力依赖服务端清单投影。
                </p>
                <div class="env-failed-actions">
                  <button
                    type="button"
                    class="filled-button"
                    :disabled="!canFreeze(data) || freezeState.kind === 'loading'"
                    @click="confirmFreeze(data)"
                  >
                    确认冻结
                  </button>
                  <button type="button" class="text-button" @click="freezeConfirmVisible = false">取消</button>
                </div>
              </div>

              <div v-if="freezeDiagnostic" class="freeze-result">
                <DiagnosticBanner
                  :code="freezeDiagnostic.code"
                  :message="freezeDiagnostic.message"
                  :retryable="freezeDiagnostic.retryable"
                  severity="error"
                  @retry="retryFreeze"
                />
              </div>

              <div v-if="freezeEvidenceFor(data)" class="evidence-card">
                <div class="grant-row">
                  <span>提交 ID</span>
                  <code>{{ freezeEvidenceFor(data)?.submissionId }}</code>
                </div>
                <div class="grant-row">
                  <span>Object Version</span>
                  <code>{{ freezeEvidenceFor(data)?.object.objectVersion }}</code>
                </div>
                <div class="grant-row">
                  <span>SHA-256</span>
                  <code>{{ freezeEvidenceFor(data)?.object.sha256 }}</code>
                </div>
                <div class="grant-row">
                  <span>Media Type</span>
                  <code>{{ freezeEvidenceFor(data)?.object.mediaType }}</code>
                </div>
                <div class="grant-row">
                  <span>大小</span>
                  <code>{{ freezeEvidenceFor(data)?.object.sizeBytes }} B</code>
                </div>
                <p class="evidence-hint">提交凭据将保留显示，可与「评测结果」页对账。</p>
              </div>
            </div>

            <div v-if="operations.operations.kind === 'success' && operations.operations.data.length > 0" class="timeline-section">
              <h4 class="section-subtitle">操作与诊断时间线</h4>
              <EventTimeline :events="operationTimeline" aria-label="环境操作与诊断时间线" />
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
import { useEnvironmentOperations } from '@/composables/useEnvironmentOperations'
import { freezeSubmission, getFrozenSubmission, retryEnvironment } from '@/generated/contracts'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import ConsolePanel from '@/components/console/ConsolePanel.vue'
import CopyButton from '@/components/common/CopyButton.vue'
import DataTable from '@/components/common/DataTable.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import EventTimeline from '@/components/common/EventTimeline.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { formatTimestamp, idempotencyKey, ifMatch } from '@/utils/format'
import { environmentStateLabel, endpointHealthLabel, accessGrantStateLabel } from '@/utils/stateLabels'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import type { DataTableColumn } from '@/components/common/DataTable.vue'
import type {
  EndpointGrant,
  EnvironmentInstanceSchema,
  EnvironmentOperationSnapshotSchema,
  EnvironmentTemplateReleaseViewSchema,
  OperationAccepted,
} from '@/generated/contracts'
import type { TimelineEvent } from '@/components/common/EventTimeline.vue'
import {
  buildSshCommand,
  formatExpiry,
  resolveConnectUrl,
  type AccessGrantWithGateway,
  type EnvironmentInstanceWithFreeze,
} from '@/types/access'

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
const lifecycleDiagnostic = ref<DiagnosticViewModel | null>(null)
const createGrantDiagnostic = ref<DiagnosticViewModel | null>(null)

interface LifecycleTarget {
  environmentId: string
  revision: number
  action: 'start' | 'stop' | 'restart' | 'delete'
}
const lastLifecycleTarget = ref<LifecycleTarget | null>(null)

const env = useEnvironmentInstance(selectedEnvironmentId)
const access = useEnvironmentAccess(
  selectedEnvironmentId,
  computed(() => (env.instance.kind === 'success' ? env.instance.data.revision : undefined)),
  courseId,
)
const operations = useEnvironmentOperations(selectedEnvironmentId)

const freezeState = ref<AsyncState<OperationAccepted>>({ kind: 'idle' })
const freezeDiagnostic = ref<DiagnosticViewModel | null>(null)
const lastFreezeEnvironmentId = ref<string | null>(null)
// Freeze evidence is deliberately component-local: the environment instance is
// replaced wholesale by polling, and the public contract does not embed freeze
// evidence on the instance, so injecting it there would silently disappear.
const frozenSubmission = ref<{
  environmentId: string
  submissionId: string
  object: EnvironmentInstanceSchema['cleanupEvidence']
  manifestSha256: string
  frozenAt: string
} | null>(null)
const retryDiagnostic = ref<DiagnosticViewModel | null>(null)
const retryingEnvironment = ref(false)
const freezeConfirmVisible = ref(false)
// Until the public contract exposes a server-owned SubmissionSpec projection
// for the selected release, the freeze manifest is the platform default
// workspace rule. It is rendered for student confirmation before freezing and
// is the exact object sent to the freeze endpoint (needs-contract: #178).
const FREEZE_MANIFEST = {
  apiVersion: 'evaluation.labweaver.io/v1',
  kind: 'SubmissionManifest',
  name: 'workspace-freeze',
  include: [{ kind: 'exactFile', path: 'README.md' }],
  exclude: [],
  required: [{ kind: 'exactFile', path: 'README.md' }],
  llmReadable: [],
  followSymlinks: false,
  maxFiles: 1000,
  maxTotalBytes: 10485760,
  source: 'workspace',
} as const
// One Idempotency-Key per logical intent, kept across retries until the intent
// reaches a terminal outcome, so a network timeout + retry cannot mint a
// duplicate frozen submission or a duplicate environment.
const freezeIntentKey = ref<string | null>(null)
const createIntentKey = ref<string | null>(null)
const createIntentReleaseId = ref<string | null>(null)

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
    lifecycleDiagnostic.value = null
    createGrantDiagnostic.value = null
    if (id) void Promise.all([access.loadEndpoints(), access.loadCurrentGrant()])
  },
  { immediate: true },
)

watch(
  () =>
    env.instance.kind === 'success'
      ? `${env.instance.data.id}:${env.instance.data.revision}:${env.instance.data.observedState}`
      : null,
  (identity, previousIdentity) => {
    if (identity && identity !== previousIdentity) {
      void Promise.all([access.loadEndpoints(), access.loadCurrentGrant()])
    }
  },
)

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

const operationTimeline = computed<TimelineEvent[]>(() => {
  if (operations.operations.kind !== 'success') return []
  return operations.operations.data.map((op: EnvironmentOperationSnapshotSchema) => ({
    id: op.operationId,
    title: `${op.kind} · ${op.state}`,
    timestamp: op.terminalAt ?? op.startedAt ?? op.acceptedAt,
    description: op.diagnosticCode
      ? `diagnostic: ${op.diagnosticCode}, revision: ${op.currentRevision}`
      : `revision: ${op.currentRevision}`,
  }))
})

const PROGRESSION_STEPS = ['已受理', '校验中', '构建中', '置备中', '运行中'] as const

const activeOperation = computed<EnvironmentOperationSnapshotSchema | null>(() => {
  if (operations.operations.kind !== 'success') return null
  const items = operations.operations.data
  const active = items.filter((op) => op.state === 'accepted' || op.state === 'running')
  const pool = active.length > 0 ? active : items
  return [...pool].sort((left, right) => right.acceptedAt.localeCompare(left.acceptedAt))[0] ?? null
})

const progressionStepIndex = computed(() => {
  const instance = env.instance.kind === 'success' ? env.instance.data : undefined
  if (!instance) return null
  const phase = activeOperation.value?.providerPhase ?? null
  if (phase === 'validating' || phase === 'building' || phase === 'provisioning') {
    return { validating: 1, building: 2, provisioning: 3 }[phase]
  }
  switch (instance.observedState) {
    case 'requested':
      return 0
    case 'validating':
      return 1
    case 'building':
      return 2
    case 'provisioning':
      return 3
    case 'ready':
      return 4
    default:
      return null
  }
})

const showProgression = computed(() => {
  const index = progressionStepIndex.value
  return index !== null && index < PROGRESSION_STEPS.length - 1
})

function endpointGrantOf(g: AccessGrantWithGateway, protocol: 'https' | 'ssh') {
  return g.endpointGrants.find((eg) => eg.protocol === protocol && eg.health === 'healthy') ?? null
}

function httpsGrant(g: AccessGrantWithGateway) {
  return endpointGrantOf(g, 'https')
}

function sshGrant(g: AccessGrantWithGateway) {
  return endpointGrantOf(g, 'ssh')
}

function connectUrl(g: AccessGrantWithGateway): string | null {
  const eg = httpsGrant(g)
  return eg ? resolveConnectUrl(eg) : null
}

function openContainerRuntime(g: AccessGrantWithGateway) {
  const url = connectUrl(g)
  if (url) window.open(url, '_blank', 'noopener,noreferrer')
}

function sshCommand(g: AccessGrantWithGateway): string | null {
  const eg = sshGrant(g)
  return eg ? buildSshCommand(eg) : null
}

function sshFingerprint(g: AccessGrantWithGateway): string | null {
  return sshGrant(g)?.sshGatewayHostKeyFingerprint ?? null
}

function canFreeze(data: EnvironmentInstanceSchema): boolean {
  return data.observedState === 'ready' && freezeState.value.kind !== 'loading'
}

const freezeManifestText = JSON.stringify(FREEZE_MANIFEST, null, 2)

async function confirmFreeze(data: EnvironmentInstanceSchema) {
  freezeConfirmVisible.value = false
  await freeze(data)
}

async function freeze(data: EnvironmentInstanceSchema) {
  freezeDiagnostic.value = null
  lastFreezeEnvironmentId.value = data.id
  // Reuse the same idempotency key across retries of this freeze intent; it is
  // cleared once the freeze reaches a terminal outcome.
  if (!freezeIntentKey.value || lastFreezeEnvironmentId.value !== data.id) {
    freezeIntentKey.value = idempotencyKey()
  }
  const intentKey = freezeIntentKey.value
  freezeState.value = { kind: 'loading', message: '冻结提交中…' }
  const result = await freezeSubmission({
    path: { environmentId: data.id },
    headers: { 'Idempotency-Key': intentKey, 'If-Match': ifMatch(data.revision) },
    body: {
      courseId: data.courseId,
      manifest: FREEZE_MANIFEST,
    },
  })
  if (result.error) {
    const problem = extractProblemDetails(result.error)
    freezeState.value = {
      kind: 'error',
      diagnostic: makeDiagnostic(
        problem?.diagnosticCode ?? 'FREEZE_FAILED',
        problem?.detail ?? '冻结提交失败',
        problem?.retryable ?? true,
      ),
    }
    freezeDiagnostic.value = freezeState.value.kind === 'error' ? freezeState.value.diagnostic : null
    freezeIntentKey.value = null
    return
  }
  freezeIntentKey.value = null
  freezeState.value = { kind: 'success', data: result.data }
  const submissionId = result.data.statusUrl.match(
    /^\/api\/v1\/frozen-submissions\/([0-9a-f-]{36})$/,
  )?.[1]
  if (!submissionId) {
    freezeState.value = {
      kind: 'error',
      diagnostic: makeDiagnostic(
        'FREEZE_STATUS_IDENTITY_INVALID',
        '冻结提交返回了无法验证的状态地址',
        false,
      ),
    }
    freezeDiagnostic.value = freezeState.value.diagnostic
    freezeIntentKey.value = null
    return
  }
  let frozenObject: EnvironmentInstanceWithFreeze['freezeEvidence']
  let frozenManifestSha256: string | null = null
  let frozenAt: string | null = null
  for (let attempt = 0; attempt < 30; attempt += 1) {
    const frozen = await getFrozenSubmission({ path: { submissionId } })
    if (frozen.data) {
      frozenObject = frozen.data.object
      frozenManifestSha256 = frozen.data.manifestSha256
      frozenAt = frozen.data.frozenAt
      break
    }
    const problem = extractProblemDetails(frozen.error)
    if (
      problem &&
      problem.diagnosticCode !== 'LW_COLLECT_SUBMISSION_NOT_FOUND' &&
      !problem.retryable
    ) {
      freezeState.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem.diagnosticCode ?? 'FREEZE_READBACK_FAILED',
          problem.detail ?? '冻结提交读取失败',
          false,
        ),
      }
      freezeDiagnostic.value = freezeState.value.diagnostic
      freezeIntentKey.value = null
      return
    }
    await new Promise<void>((resolve) => window.setTimeout(resolve, 1000))
  }
  if (!frozenObject) {
    freezeState.value = {
      kind: 'error',
      diagnostic: makeDiagnostic('FREEZE_READBACK_TIMEOUT', '冻结提交读取超时', true),
    }
    freezeDiagnostic.value = freezeState.value.diagnostic
    return
  }
  frozenSubmission.value = {
    environmentId: data.id,
    submissionId,
    object: frozenObject,
    manifestSha256: frozenManifestSha256 ?? '',
    frozenAt: frozenAt ?? '',
  }
  await env.load()
  await operations.load()
}

function freezeEvidenceFor(data: EnvironmentInstanceSchema) {
  return frozenSubmission.value && frozenSubmission.value.environmentId === data.id
    ? frozenSubmission.value
    : null
}

async function retryFreeze() {
  const id = lastFreezeEnvironmentId.value
  if (!id) return
  const instance = env.instance.kind === 'success' ? env.instance.data : undefined
  if (instance && instance.id === id) {
    await freeze(instance)
  }
}

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
  // Keep one idempotency key per create intent (release + user) so a retry
  // after a timeout replays the same create instead of minting a duplicate.
  if (createIntentReleaseId.value !== release.id || !createIntentKey.value) {
    createIntentKey.value = idempotencyKey()
    createIntentReleaseId.value = release.id
  }
  const intentKey = createIntentKey.value
  lifecycle.operating.add(`create:${release.id}`)
  try {
    const result = await lifecycle.create(
      {
        courseId: courseId.value ?? '',
        releaseId: release.id,
        releaseVersion: release.version,
      },
      intentKey,
    )
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

async function runLifecycle(data: EnvironmentInstanceSchema, action: LifecycleTarget['action']) {
  lifecycleDiagnostic.value = null
  lastLifecycleTarget.value = { environmentId: data.id, revision: data.revision, action }
  const result = await lifecycle.act(data.id, data.revision, action)
  if (!result.ok) {
    lifecycleDiagnostic.value = result.diagnostic ?? null
    return
  }
  // Refresh the operations timeline immediately so the new command shows up
  // instead of waiting for the next full page load.
  await operations.load()
}

async function retryFailedOperation(data: EnvironmentInstanceSchema) {
  retryDiagnostic.value = null
  retryingEnvironment.value = true
  try {
    const result = await retryEnvironment({
      path: { environmentId: data.id },
      headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(data.revision) },
    })
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      retryDiagnostic.value = makeDiagnostic(
        problem?.diagnosticCode ?? 'ENVIRONMENT_RETRY_FAILED',
        problem?.detail ?? '重试失败的操作未成功',
        problem?.retryable ?? true,
      )
      return
    }
    await Promise.all([env.load(), operations.load()])
  } finally {
    retryingEnvironment.value = false
  }
}

async function retryLifecycle() {
  const target = lastLifecycleTarget.value
  if (!target) return
  const { environmentId, revision, action } = target
  if (action === 'delete') {
    const instance = env.instance.kind === 'success' ? env.instance.data : undefined
    if (instance && instance.id === environmentId) {
      await runLifecycle(instance, 'delete')
    }
    return
  }
  const instance = env.instance.kind === 'success' ? env.instance.data : undefined
  if (instance && instance.id === environmentId) {
    await runLifecycle(instance, action)
  }
}

function openDelete(data: EnvironmentInstanceSchema) {
  deleteEnvironment.value = data
}

async function confirmDeleteEnvironment() {
  if (!deleteEnvironment.value) return
  const data = deleteEnvironment.value
  deleteEnvironment.value = null
  await runLifecycle(data, 'delete')
}

async function issueAccessGrant() {
  createGrantDiagnostic.value = null
  const result = await access.createGrant()
  if (result && !result.ok && result.diagnostic) {
    createGrantDiagnostic.value = result.diagnostic
  }
}

async function revokeAccessGrant() {
  createGrantDiagnostic.value = null
  const result = await access.revokeGrant()
  if (!result.ok && result.diagnostic) {
    createGrantDiagnostic.value = result.diagnostic
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

.lifecycle-result {
  margin-top: 16px;
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

.env-progression {
  margin-bottom: 16px;
}

.progression-steps {
  list-style: none;
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  margin: 0 0 6px;
  padding: 0;
}

.progression-step {
  position: relative;
  padding: 4px 12px;
  border-radius: var(--md-sys-shape-small);
  font: var(--md-sys-label-medium);
  color: var(--md-sys-color-on-surface-variant);
  background: var(--md-sys-color-surface-container-high);
}

.progression-step--done {
  background: var(--md-sys-color-tertiary-container);
  color: var(--md-sys-color-on-tertiary-container);
}

.progression-step--active {
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
}

.progression-meta {
  margin: 0;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.env-failed-panel {
  margin-bottom: 16px;
}

.env-failed-actions {
  display: flex;
  gap: 8px;
  margin-top: 8px;
}

.evidence-hint {
  margin: 8px 0 0;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.freeze-confirm {
  margin-top: 12px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  padding: 12px 16px;
}

.freeze-manifest {
  margin: 8px 0;
  padding: 12px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container);
  color: var(--md-sys-color-on-surface);
  font-family: monospace;
  font-size: 12px;
  overflow-x: auto;
  white-space: pre;
}

.freeze-confirm-hint {
  margin: 0 0 8px;
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

.console-section {
  margin-top: 20px;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.grant-actions {
  display: flex;
  gap: 12px;
  margin-top: 16px;
}

.grant-result {
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

.runtime-access {
  display: flex;
  flex-direction: column;
  gap: 16px;
  margin-top: 20px;
}

.access-card {
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.access-card__title {
  display: flex;
  align-items: center;
  gap: 8px;
  font: var(--md-sys-title-small);
  color: var(--md-sys-color-on-surface);
  margin: 0 0 8px;
}

.access-card__desc {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  margin: 0 0 12px;
}

.access-card__hint {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-error);
  margin: 8px 0 0;
}

.ssh-command {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.ssh-command__text {
  padding: 8px 12px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container-highest);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
  word-break: break-all;
}

.ssh-meta {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-top: 12px;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.freeze-section {
  margin-top: 24px;
}

.freeze-desc {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  margin: 0 0 12px;
}

.freeze-result {
  margin-top: 16px;
}

.evidence-card {
  margin-top: 16px;
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}
</style>
