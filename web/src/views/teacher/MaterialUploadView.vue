<template>
  <div class="material-upload">
    <header class="page-header">
      <h2>材料上传与 AgentRun</h2>
      <p class="page-subtitle">
        上传题面、Starter 和样例，确认项目 LLM 出站策略后启动 AgentRun。
      </p>
    </header>

    <DiagnosticBanner
      v-if="!projectId"
      code="PROJECT_CONTEXT_MISSING"
      message="请先在顶部项目选择器中选择一个项目。"
      :retryable="false"
      severity="error"
    />

    <DiagnosticBanner
      v-if="restoreDiagnostic"
      :code="restoreDiagnostic.code"
      :message="restoreDiagnostic.message"
      :retryable="restoreDiagnostic.retryable"
      severity="warning"
      @retry="retryRestore"
    />
    <p
      v-if="restoreDiagnostic && diagnosticAction(restoreDiagnostic.code)"
      class="diagnostic-action"
      role="status"
    >
      建议：{{ diagnosticAction(restoreDiagnostic.code) }}
    </p>

    <section
      class="policy-section"
      aria-labelledby="policy-heading"
    >
      <h3
        id="policy-heading"
        class="section-title"
      >
        <SvgIcon
          name="policy"
          size="sm"
          aria-hidden="true"
        />
        项目 AI 设置
      </h3>
      <AsyncStateView
        v-if="projectId"
        :state="policy.state"
        @retry="policy.load"
      >
        <template #success="{ data }">
          <div class="policy-card md-card">
            <div class="policy-primary">
              <div>
                <strong>已配置项目 AI 设置</strong>
                <p>材料只会按项目已批准的策略提交给生成服务。</p>
              </div>
              <span class="state-chip state-chip--ready">可用</span>
            </div>
            <details class="technical-details">
              <summary>查看项目 AI 设置</summary>
              <div class="policy-details">
                <div class="policy-row">
                  <span class="policy-label">模型</span>
                  <span class="policy-value">{{ data.binding.model }}</span>
                </div>
                <div class="policy-row">
                  <span class="policy-label">Claude Code 版本</span>
                  <span class="policy-value">{{ data.binding.claudeCodeVersion }}</span>
                </div>
                <div class="policy-row">
                  <span class="policy-label">运行时配置</span>
                  <code class="policy-value">{{ data.binding.runtimeBinding }}</code>
                </div>
                <div class="policy-row">
                  <span class="policy-label">硬拒绝分类</span>
                  <span class="policy-tags">
                    <span
                      v-for="cls in data.deniedDataClasses"
                      :key="cls"
                      class="tag tag--deny"
                    >{{ cls }}</span>
                  </span>
                </div>
                <div class="policy-row">
                  <span class="policy-label">单次预算</span>
                  <span class="policy-value">
                    {{ data.budget.maxInputTokens }} / {{ data.budget.maxOutputTokens }} tokens，
                    {{ data.budget.maxRequests }} 请求，{{ data.budget.timeoutMilliseconds }} ms
                  </span>
                </div>
                <div class="policy-row">
                  <span class="policy-label">策略版本</span>
                  <span class="policy-value">rev-{{ data.revision }} / {{ data.id }}</span>
                </div>
              </div>
            </details>
          </div>
        </template>
      </AsyncStateView>
    </section>

    <section
      class="upload-section"
      aria-labelledby="upload-heading"
    >
      <h3
        id="upload-heading"
        class="section-title"
      >
        <SvgIcon
          name="upload"
          size="sm"
          aria-hidden="true"
        />
        材料包
      </h3>

      <div
        class="drop-zone"
        :class="{ 'drop-zone--active': dragOver }"
        @dragenter.prevent="dragOver = true"
        @dragover.prevent="dragOver = true"
        @dragleave.prevent="dragOver = false"
        @drop.prevent="onDrop"
      >
        <input
          ref="fileInput"
          type="file"
          webkitdirectory
          directory
          multiple
          class="file-input"
          @change="onFileInput"
        >
        <SvgIcon
          name="folder_open"
          size="lg"
          aria-hidden="true"
        />
        <p>拖拽文件夹到此处，或点击选择材料文件夹</p>
        <button
          type="button"
          class="outlined-button"
          @click="fileInput?.click()"
        >
          选择文件夹
        </button>
      </div>

      <div
        v-if="upload.files.length > 0"
        class="file-list"
      >
        <DataTable
          :columns="fileColumns"
          :rows="upload.files"
          aria-label="待上传材料文件"
        >
          <template #path="{ row }">
            <span class="file-path">{{ row.path }}</span>
          </template>
          <template #sizeBytes="{ row }">
            {{ upload.formatBytes(row.sizeBytes) }}
          </template>
          <template #status="{ row }">
            <span
              class="file-status"
              :class="`file-status--${row.status}`"
            >
              <template v-if="row.status === 'pending'">待上传</template>
              <template v-else-if="row.status === 'uploading'">上传中 {{ row.progress }}%</template>
              <template v-else-if="row.status === 'done'">完成</template>
              <template v-else-if="row.status === 'error'">失败</template>
            </span>
          </template>
          <template #actions="{ row }">
            <button
              type="button"
              class="icon-button text-button"
              aria-label="移除"
              @click="upload.removeFile(row.path)"
            >
              <SvgIcon
                name="delete"
                size="sm"
                aria-hidden="true"
              />
            </button>
          </template>
        </DataTable>
        <details class="technical-details file-integrity-details">
          <summary>查看文件完整性详情</summary>
          <ul class="file-integrity-list">
            <li
              v-for="row in upload.files"
              :key="row.path"
            >
              <span>{{ row.path }}</span>
              <code :title="row.sha256">{{ row.sha256 }}</code>
            </li>
          </ul>
        </details>
      </div>

      <div
        v-if="upload.state.kind === 'error'"
        class="upload-error"
      >
        <DiagnosticBanner
          :code="upload.state.diagnostic.code"
          :message="upload.state.diagnostic.message"
          :retryable="upload.state.diagnostic.retryable"
          severity="error"
          @retry="upload.retry"
        />
        <p
          v-if="diagnosticAction(upload.state.diagnostic.code)"
          class="diagnostic-action"
          role="status"
        >
          建议：{{ diagnosticAction(upload.state.diagnostic.code) }}
        </p>
      </div>

      <div class="upload-actions">
        <button
          type="button"
          class="filled-button"
          :disabled="!canUpload"
          @click="upload.createSession"
        >
          <template v-if="upload.state.kind === 'hashing'">
            计算哈希中…
          </template>
          <template v-else-if="upload.state.kind === 'loading'">
            读取已归档材料包…
          </template>
          <template v-else-if="upload.state.kind === 'creating'">
            创建会话…
          </template>
          <template v-else-if="upload.state.kind === 'uploading'">
            上传中…
          </template>
          <template v-else-if="upload.state.kind === 'completing'">
            确认归档…
          </template>
          <template v-else>
            上传材料包
          </template>
        </button>
        <button
          v-if="packageDone"
          type="button"
          class="text-button"
          @click="clearAuthoring"
        >
          清除
        </button>
      </div>

      <div
        v-if="packageDone && uploadedPackage"
        class="package-summary"
      >
        <SvgIcon
          name="check_circle"
          size="md"
          aria-hidden="true"
        />
        <div>
          <strong>材料包已归档</strong>
          <span>可以启动实验候选生成。</span>
        </div>
        <details class="technical-details package-technical-details">
          <summary>查看材料包引用</summary>
          <dl class="technical-meta">
            <dt>Package ID</dt>
            <dd><code>{{ uploadedPackage.id }}</code></dd>
            <dt>Revision</dt>
            <dd><code>rev-{{ uploadedPackage.revision }}</code></dd>
            <dt>文件数</dt>
            <dd>{{ uploadedPackage.files.length }}</dd>
          </dl>
        </details>
      </div>
    </section>

    <section
      v-if="displayRun"
      class="run-section"
      aria-labelledby="run-heading"
    >
      <h3
        id="run-heading"
        class="section-title"
      >
        <SvgIcon
          name="smart_toy"
          size="sm"
          aria-hidden="true"
        />
        生成实验候选
      </h3>

      <p class="section-subtitle">
        材料包就绪后启动生成，页面刷新后会从项目服务恢复最近一次任务。
      </p>

      <button
        type="button"
        class="filled-button"
        :disabled="!canStartRun"
        @click="startRun"
      >
        {{ agent.acting === 'start' ? '提交中…' : '启动 AgentRun' }}
      </button>
      <p
        v-if="agent.run.kind === 'success' && runIsInFlight(agent.run.data.state)"
        class="state-note"
        role="status"
      >
        当前任务正在{{ runStateLabel(agent.run.data.state) }}，完成前不能重复启动。
      </p>

      <AsyncStateView
        v-if="agent.run.kind !== 'idle'"
        :state="agent.run"
        @retry="retryCurrentRun"
      >
        <template #success="{ data }">
          <div class="run-card md-card">
            <div class="run-header">
              <div>
                <strong>候选生成任务</strong>
                <p class="run-state-summary">
                  {{ runStateLabel(data.state) }}
                </p>
              </div>
              <GcpStatusPill
                :state="data.state"
                domain="agent"
              />
            </div>
            <div
              v-if="data.tracks.length > 0"
              class="run-tracks"
            >
              <div
                v-for="track in data.tracks"
                :key="track.kind"
                class="run-track"
              >
                <div class="run-track__header">
                  <span class="run-track__kind">{{ agentTrackKindLabel(track.kind) }}</span>
                  <span class="run-track__candidate">{{ track.candidateId ? '已生成候选' : '等待生成' }}</span>
                </div>
                <p
                  v-if="attemptDiagnostic(track)"
                  class="run-track__diagnostic"
                >
                  诊断：{{ attemptDiagnostic(track) }}
                </p>
                <p
                  v-if="attemptDiagnostic(track) && diagnosticAction(attemptDiagnostic(track)!)"
                  class="diagnostic-action"
                  role="status"
                >
                  建议：{{ diagnosticAction(attemptDiagnostic(track)!) }}
                </p>
              </div>
            </div>
            <p
              v-else
              class="run-tracks-empty"
            >
              AgentRun 尚未产生轨道尝试明细。
            </p>
            <details class="technical-details run-technical-details">
              <summary>查看生成任务详情</summary>
              <dl class="technical-meta">
                <dt>AgentRun ID</dt>
                <dd><code>{{ data.id }}</code></dd>
                <dt>Revision</dt>
                <dd><code>rev-{{ data.revision }}</code></dd>
                <dt>材料包 ID</dt>
                <dd><code>{{ data.packageId }}</code></dd>
              </dl>
              <div
                v-for="track in data.tracks"
                :key="`${track.kind}-attempts`"
                class="run-attempt-group"
              >
                <h4>{{ agentTrackKindLabel(track.kind) }}详细记录</h4>
                <ul class="run-attempts">
                  <li
                    v-for="attempt in track.attempts"
                    :key="attempt.number"
                    class="run-attempt"
                  >
                    <span class="run-attempt__number">第 {{ attempt.number }} 次尝试</span>
                    <GcpStatusPill
                      :state="attempt.state"
                      domain="agent"
                      size="sm"
                    />
                    <span
                      v-if="attempt.diagnosticCode"
                      class="run-attempt__diagnostic"
                    >
                      {{ attempt.diagnosticCode }}
                    </span>
                    <span
                      v-if="attempt.usageObserved"
                      class="run-attempt__usage"
                      title="本次尝试的 LLM 用量（仅观测，不参与评分）"
                    >
                      {{ attempt.usage.inputTokens }}+{{ attempt.usage.outputTokens }} tokens
                    </span>
                  </li>
                </ul>
              </div>
            </details>
            <div class="run-actions">
              <button
                v-if="data.state === 'running' || data.state === 'requested'"
                type="button"
                class="text-button"
                @click="agent.cancel"
              >
                取消
              </button>
              <template v-if="data.state === 'failed' || data.state === 'partially_succeeded'">
                <button
                  type="button"
                  class="text-button"
                  @click="agent.retryTrack('environment')"
                >
                  重试环境轨道
                </button>
                <button
                  type="button"
                  class="text-button"
                  @click="agent.retryTrack('evaluation')"
                >
                  重试评测轨道
                </button>
              </template>
              <RouterLink
                v-if="data.state === 'succeeded' || data.state === 'partially_succeeded'"
                class="filled-button approval-link"
                :to="{ path: '/teacher/approvals', query: { projectId: projectId ?? undefined, runId: data.id } }"
              >
                进入候选审批
              </RouterLink>
            </div>
            <p
              v-if="data.state === 'partially_succeeded'"
              class="run-partial-hint"
              role="status"
            >
              部分轨道失败：已生成的候选仍可进入审批；失败轨道可重试后重新审批。
            </p>
          </div>
        </template>
      </AsyncStateView>

      <DiagnosticBanner
        v-if="agent.outcome"
        :code="agent.outcome.code"
        :message="agent.outcome.message"
        :retryable="agent.outcome.retryable"
        severity="info"
      />
      <p
        v-if="agent.outcome && diagnosticAction(agent.outcome.code)"
        class="diagnostic-action"
        role="status"
      >
        建议：{{ diagnosticAction(agent.outcome.code) }}
      </p>
      <p
        v-if="agent.run.kind === 'error' && diagnosticAction(agent.run.diagnostic.code)"
        class="diagnostic-action"
        role="status"
      >
        建议：{{ diagnosticAction(agent.run.diagnostic.code) }}
      </p>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import { useProjects } from '@/composables/useProjects'
import { useActiveProjectLlmPolicy } from '@/composables/useActiveProjectLlmPolicy'
import { useProjectProblemPackageUpload } from '@/composables/useProjectProblemPackageUpload'
import { useProjectAgentRun } from '@/composables/useProjectAgentRun'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import DataTable from '@/components/common/DataTable.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import GcpStatusPill from '@/components/common/GcpStatusPill.vue'
import { agentTrackKindLabel } from '@/utils/stateLabels'
import type { DataTableColumn } from '@/components/common/DataTable.vue'
import type { AgentRunSchema } from '@/generated/contracts'
import type { UploadFile } from '@/composables/useProjectProblemPackageUpload'
import { makeDiagnostic, type DiagnosticViewModel } from '@/types/async'

// Keep the view renderable in the small, router-less unit mounts used for
// upload-state tests. The production route/router are always provided by the
// app shell; the local no-op only protects those isolated mounts from trying
// to read an absent injection.
const route = useRoute() ?? ({ query: {} } as ReturnType<typeof useRoute>)
const router = useRouter() ?? ({ replace: async () => undefined } as unknown as ReturnType<typeof useRouter>)
const projects = useProjects()
const projectId = computed(() => projects.selectedProjectId)
const courseId = computed(() => projects.selectedProject?.courseId ?? null)
const policy = useActiveProjectLlmPolicy(projectId)
const policyRevision = computed(() => (policy.state.kind === 'success' ? policy.state.data.revision : undefined))
const upload = useProjectProblemPackageUpload(projectId, policyRevision, courseId)
const agent = useProjectAgentRun(projectId)

const fileInput = ref<HTMLInputElement | null>(null)
const dragOver = ref(false)
const fileColumns: DataTableColumn<UploadFile>[] = [
  { key: 'path', title: '路径' },
  { key: 'sizeBytes', title: '大小' },
  { key: 'status', title: '状态' },
  { key: 'actions', title: '操作' },
]

const canUpload = computed(() => {
  const ready = upload.state.kind === 'ready' || upload.state.kind === 'error'
  return ready && upload.files.length > 0 && policyRevision.value !== undefined
})

const routeProjectId = computed(() => route.query.projectId?.toString().trim() || undefined)
const routePackageId = computed(() => route.query.packageId?.toString().trim() || undefined)
const routeRunId = computed(() => route.query.runId?.toString().trim() || undefined)
let restoredContextKey = ''
let restoreGeneration = 0
const restoreDiagnostic = ref<DiagnosticViewModel | null>(null)

const packageDone = computed(() => upload.state.kind === 'done')
const uploadedPackage = computed(() => (upload.state.kind === 'done' ? upload.state.package : null))
const IN_FLIGHT_RUN_STATES = ['requested', 'running', 'cancelling', 'awaiting_approval'] as const

const displayRun = computed(() => {
  if (routeRunId.value) return true
  if (!packageDone.value) return false
  return agent.run.kind !== 'success' || agent.run.data.packageId === uploadedPackage.value?.id
})

const canStartRun = computed(() => {
  if (!packageDone.value || !uploadedPackage.value || policy.state.kind !== 'success' || agent.acting) return false
  if (agent.run.kind !== 'success' || agent.run.data.packageId !== uploadedPackage.value.id) return true
  return !runIsInFlight(agent.run.data.state)
})

function runIsInFlight(state: AgentRunSchema['state']): boolean {
  return IN_FLIGHT_RUN_STATES.includes(state as (typeof IN_FLIGHT_RUN_STATES)[number])
}

function runStateLabel(state: AgentRunSchema['state']): string {
  return ({
    requested: '已提交',
    running: '生成中',
    partially_succeeded: '部分完成',
    succeeded: '生成完成',
    awaiting_approval: '等待审核',
    failed: '生成失败',
    cancelling: '取消中',
    cancelled: '已取消',
  } as Record<AgentRunSchema['state'], string>)[state]
}

function diagnosticAction(code: string): string | null {
  switch (code) {
    case 'LW_RESOURCE_EXHAUSTED':
      return 'AI 生成预算可能因输入或输出 token、调用次数达到项目上限而不足；当前诊断未说明具体项，也未提供已用量或上限。请缩短材料后重试，或请管理员检查项目 AI 预算与调用限制。'
    case 'REVISION_CONFLICT':
      return '当前页面使用的材料或策略已经变化，请刷新页面后重新检查。'
    case 'LW_ACCESS_DENIED':
      return '请确认当前账号仍有该项目的教师权限，并从可访问项目重新开始。'
    case 'LW_CANDIDATE_NOT_FOUND':
      return '候选可能仍在服务端同步，点击重试继续读取；超时后请重新打开该 AgentRun。'
    case 'PROJECT_APPROVAL_RUN_KIND_UNSUPPORTED':
      return '请从材料上传页启动实验候选生成，不要使用 Work 配置或其他用途的运行记录。'
    case 'PROJECT_RUN_STALE_CONTEXT':
    case 'UPLOAD_RUN_PACKAGE_MISMATCH':
    case 'PROJECT_APPROVAL_STALE_CONTEXT':
    case 'UPLOAD_PACKAGE_STALE_CONTEXT':
      return '请从当前项目重新选择材料并启动任务，旧链接不会被当作当前项目状态。'
    default:
      return null
  }
}

function updateAuthoringRoute(next: { packageId?: string; runId?: string }) {
  const query = {
    ...route.query,
    projectId: projectId.value ?? undefined,
    packageId: next.packageId,
    runId: next.runId,
  }
  void router.replace({ query })
}

function clearAuthoring() {
  restoreGeneration += 1
  restoredContextKey = ''
  restoreDiagnostic.value = null
  upload.clear()
  updateAuthoringRoute({})
}

function attemptDiagnostic(track: AgentRunSchema['tracks'][number]): string | null {
  return [...track.attempts].reverse().find((attempt) => attempt.diagnosticCode)?.diagnosticCode ?? null
}

async function restoreAuthoringContext(generation = ++restoreGeneration) {
  const id = projectId.value
  if (!id) return
  const packageId = routePackageId.value
  const runId = routeRunId.value
  const contextKey = `${id}:${packageId ?? ''}:${runId ?? ''}`
  if (contextKey === restoredContextKey) return
  restoredContextKey = contextKey
  restoreDiagnostic.value = null
  const isCurrent = () => (
    generation === restoreGeneration
    && projectId.value === id
    && routePackageId.value === packageId
    && routeRunId.value === runId
  )

  if (runId) {
    await agent.load(runId)
    if (!isCurrent()) return
    if (agent.run.kind !== 'success') return
    if (agent.run.data.id !== runId || agent.run.data.projectId !== id) {
      upload.clear()
      restoreDiagnostic.value = makeDiagnostic(
        'PROJECT_APPROVAL_STALE_CONTEXT',
        '生成任务返回的项目引用已变化，已停止恢复以避免显示过期任务。请从当前项目重新打开材料页。',
        false,
      )
      return
    }
    if (agent.run.data.purpose.kind !== 'authoring' || agent.run.data.purpose.environmentClass !== 'experiment') {
      upload.clear()
      restoreDiagnostic.value = makeDiagnostic(
        'PROJECT_APPROVAL_RUN_KIND_UNSUPPORTED',
        '该运行记录不是实验包生成任务，已停止恢复。请从材料页启动实验候选生成。',
        false,
      )
      return
    }
    if (packageId && agent.run.data.packageId !== packageId) {
      upload.clear()
      restoreDiagnostic.value = makeDiagnostic(
        'UPLOAD_RUN_PACKAGE_MISMATCH',
        '链接中的材料包与生成任务不一致，已停止恢复以避免混用。请从材料页重新选择项目和任务。',
        false,
      )
      return
    }
  }
  if (packageId) {
    await upload.loadPackage(packageId)
    if (!isCurrent()) return
    if (upload.state.kind === 'error') return
  }
  if (runId) {
    if (!packageId && agent.run.kind === 'success') {
      // The run is the authoritative link to its completed package. Persisting
      // that id in the route makes the next refresh deterministic while the
      // package endpoint still validates project ownership.
      await upload.loadPackage(agent.run.data.packageId)
      if (!isCurrent()) return
      if (upload.state.kind === 'done') {
        restoredContextKey = `${id}:${agent.run.data.packageId}:${runId}`
        updateAuthoringRoute({ packageId: agent.run.data.packageId, runId })
      }
    }
  }
}

function retryRestore() {
  restoredContextKey = ''
  void restoreAuthoringContext()
}

watch(
  () => projects.projects,
  (state) => {
    if (state.kind !== 'success' || !routeProjectId.value) return
    if (state.data.some((project) => project.id === routeProjectId.value)) projects.select(routeProjectId.value)
  },
  { immediate: true },
)

watch([projectId, routePackageId, routeRunId], () => void restoreAuthoringContext(), { immediate: true })

watch(projectId, (id, previousId) => {
  if (!id || !previousId || id === previousId) return
  const followsRouteProject = id === routeProjectId.value
  restoreGeneration += 1
  restoredContextKey = ''
  restoreDiagnostic.value = null
  upload.clear()
  if (followsRouteProject) void restoreAuthoringContext()
  else updateAuthoringRoute({})
})

watch(
  () => upload.state,
  (state) => {
    if (state.kind !== 'done' || !projectId.value) return
    const packageId = state.package.id
    const currentRun = agent.run.kind === 'success' ? agent.run.data : null
    const runId = routeRunId.value
      && (currentRun
        ? currentRun.id === routeRunId.value && currentRun.packageId === packageId
        : routePackageId.value === packageId)
      ? routeRunId.value
      : undefined
    if (routePackageId.value !== packageId || routeRunId.value !== runId) {
      restoredContextKey = `${projectId.value}:${packageId}:${runId ?? ''}`
      updateAuthoringRoute({ packageId, runId })
    }
  },
)

function onFileInput(event: Event) {
  const target = event.target as HTMLInputElement
  if (target.files && target.files.length > 0) {
    upload.addFiles(Array.from(target.files))
  }
  target.value = ''
}

function onDrop(event: DragEvent) {
  dragOver.value = false
  if (event.dataTransfer) {
    upload.addDirectoryItems(event.dataTransfer.items)
  }
}

async function startRun() {
  const pkg = uploadedPackage.value
  const policyData = policy.state.kind === 'success' ? policy.state.data : undefined
  if (!pkg || !policyData || !canStartRun.value) return
  const started = await agent.start({
    packageId: pkg.id,
    packageRevision: pkg.revision,
    policyId: policyData.id,
    policyRevision: policyData.revision,
    environmentClass: 'experiment',
  })
  if (started && agent.run.kind === 'success') {
    restoredContextKey = `${projectId.value}:${pkg.id}:${agent.run.data.id}`
    updateAuthoringRoute({ packageId: pkg.id, runId: agent.run.data.id })
  }
}

async function retryCurrentRun() {
  const current = agent.run.kind === 'success' ? agent.run.data : undefined
  if (current) {
    await agent.load(current.id)
  }
}

// Release background work when leaving the page: stop the AgentRun poll
// timer and close the SSE stream so neither outlives the view.
onUnmounted(() => {
  agent.stopPolling()
})
</script>

<style scoped>
.material-upload {
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

.policy-card,
.run-card {
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.policy-primary,
.run-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

.policy-primary strong,
.run-header strong {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-small);
}

.policy-primary p,
.run-state-summary {
  margin: 4px 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.state-chip {
  display: inline-flex;
  align-items: center;
  width: fit-content;
  padding: 3px 9px;
  border-radius: var(--md-sys-shape-full);
  font: var(--md-sys-label-small);
}

.state-chip--ready {
  background: var(--md-sys-color-tertiary-container);
  color: var(--md-sys-color-on-tertiary-container);
}

.technical-details {
  margin-top: 14px;
}

.technical-details summary {
  color: var(--md-sys-color-primary);
  cursor: pointer;
  font: var(--md-sys-label-large);
}

.policy-details {
  margin-top: 8px;
}

.technical-meta {
  display: grid;
  grid-template-columns: minmax(110px, 150px) minmax(0, 1fr);
  gap: 7px 14px;
  margin: 10px 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.technical-meta dt,
.technical-meta dd {
  min-width: 0;
  margin: 0;
}

.technical-meta dd {
  color: var(--md-sys-color-on-surface);
  overflow-wrap: anywhere;
}

.policy-row {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 8px 0;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.policy-row:last-child {
  border-bottom: none;
}

.policy-label {
  width: 120px;
  flex-shrink: 0;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.policy-value {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface);
  word-break: break-all;
}

.policy-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.tag {
  padding: 4px 10px;
  border-radius: var(--md-sys-shape-small);
  font: var(--md-sys-label-medium);
}

.tag--deny {
  background: var(--md-sys-color-error-container);
  color: var(--md-sys-color-on-error-container);
}

.drop-zone {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 32px;
  border: 2px dashed var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-large);
  background: var(--md-sys-color-surface-container-low);
  transition: border-color 0.2s, background 0.2s;
}

.drop-zone--active {
  border-color: var(--md-sys-color-primary);
  background: var(--md-sys-color-primary-container);
}

.file-input {
  display: none;
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

.filled-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.approval-link {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  text-decoration: none;
}

.text-button {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 12px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.icon-button {
  width: 32px;
  height: 32px;
  padding: 0;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  cursor: pointer;
}

.file-list {
  margin-top: 16px;
}

.file-integrity-details {
  margin-top: 12px;
}

.file-integrity-list {
  display: grid;
  gap: 8px;
  margin: 10px 0 0;
  padding: 0;
  list-style: none;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.file-integrity-list li {
  display: grid;
  grid-template-columns: minmax(120px, 220px) minmax(0, 1fr);
  gap: 12px;
}

.file-integrity-list code {
  overflow-wrap: anywhere;
}

.file-path {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface);
}

.file-status--pending { color: var(--md-sys-color-on-surface-variant); }
.file-status--uploading { color: var(--md-sys-color-primary); }
.file-status--done { color: var(--md-sys-color-tertiary); }
.file-status--error { color: var(--md-sys-color-error); }

.upload-actions {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-top: 16px;
}

.upload-error {
  margin-top: 16px;
}

.package-summary {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 12px;
  padding: 10px 12px;
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-tertiary-container);
  color: var(--md-sys-color-on-tertiary-container);
  font: var(--md-sys-body-medium);
}

.package-summary > div {
  display: grid;
  gap: 2px;
}

.package-summary > div span {
  font: var(--md-sys-body-small);
}

.package-technical-details {
  margin: 0 0 0 auto;
}

.run-technical-details {
  padding-top: 2px;
}

.runtime-field {
  display: flex;
  align-items: center;
  gap: 16px;
  margin-bottom: 16px;
}

.field-label {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.runtime-options {
  display: flex;
  gap: 16px;
}

.radio-option {
  display: flex;
  align-items: center;
  gap: 8px;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface);
  cursor: pointer;
}

.run-card {
  margin-top: 16px;
}

.run-header {
  margin-bottom: 12px;
}

.run-id {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
  word-break: break-all;
}

.run-state {
  padding: 4px 12px;
  border-radius: var(--md-sys-shape-small);
  font: var(--md-sys-label-large);
  text-transform: capitalize;
}

.run-state--requested { background: var(--md-sys-color-surface-container-high); color: var(--md-sys-color-on-surface-variant); }
.run-state--running { background: var(--md-sys-color-primary-container); color: var(--md-sys-color-on-primary-container); }
.run-state--cancelling { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.run-state--succeeded { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.run-state--failed { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
.run-state--cancelled { background: var(--md-sys-color-surface-container-highest); color: var(--md-sys-color-on-surface-variant); }
.run-state--partially_succeeded { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }

.run-actions {
  display: flex;
  gap: 8px;
}

.run-elapsed {
  margin-right: auto;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.run-tracks {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-bottom: 12px;
}

.run-tracks-empty {
  margin: 0 0 12px;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.run-track {
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-small);
  padding: 8px 12px;
}

.run-track__header {
  display: flex;
  align-items: center;
  gap: 12px;
}

.run-track__kind {
  font: var(--md-sys-label-large);
  color: var(--md-sys-color-on-surface);
}

.run-track__candidate {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  word-break: break-all;
}

.run-attempt-group {
  margin-top: 14px;
}

.run-attempt-group h4 {
  margin: 0 0 7px;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-small);
}

.run-attempts {
  list-style: none;
  margin: 6px 0 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.run-attempt {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  font: var(--md-sys-body-small);
}

.run-attempt__number {
  color: var(--md-sys-color-on-surface-variant);
}

.run-attempt__state {
  padding: 2px 8px;
  border-radius: var(--md-sys-shape-small);
  font: var(--md-sys-label-medium);
}

.run-attempt__state--pending,
.run-attempt__state--running { background: var(--md-sys-color-primary-container); color: var(--md-sys-color-on-primary-container); }
.run-attempt__state--repairing { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.run-attempt__state--succeeded { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.run-attempt__state--failed { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
.run-attempt__state--cancelled { background: var(--md-sys-color-surface-container-highest); color: var(--md-sys-color-on-surface-variant); }

.run-attempt__diagnostic {
  font-family: monospace;
  color: var(--md-sys-color-error);
  word-break: break-all;
}

.run-attempt__usage {
  color: var(--md-sys-color-on-surface-variant);
}

.run-partial-hint {
  margin: 8px 0 0;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.diagnostic-action {
  margin: 8px 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.stream-state {
  margin: 12px 0 0;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.stream-empty {
  margin: 8px 0 0;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.poll-error {
  margin-top: 12px;
}

.timeline-section {
  margin-top: 20px;
}
</style>
