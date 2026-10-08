<template>
  <div class="software-page">
    <header class="page-header">
      <div>
        <h2>软件配置</h2>
        <p class="page-subtitle">为选中的 Project 生成 Work 环境候选。Agent 只在项目策略和预授权范围内工作。</p>
      </div>
      <RouterLink class="outlined-button" to="/researcher/workspaces">管理项目</RouterLink>
    </header>

    <section class="project-strip md-card">
      <label>
        <span>项目</span>
        <select v-model="selectedProjectId" class="text-input" :disabled="projects.projects.kind !== 'success'">
          <option v-for="project in projectOptions" :key="project.id" :value="project.id">{{ project.name }}</option>
        </select>
      </label>
      <div v-if="selectedProject" class="project-summary">
        <span class="state-chip" :class="`state-chip--${selectedProject.state}`">{{ selectedProject.state === 'active' ? '可用' : '已归档' }}</span>
        <span>{{ selectedProject.courseId ? '课程项目' : '独立科研项目' }}</span>
        <details class="advanced-details">
          <summary>查看项目标识</summary>
          <small>项目 ID：{{ selectedProject.id }}<template v-if="selectedProject.courseId"> · 课程 ID：{{ selectedProject.courseId }}</template></small>
        </details>
      </div>
    </section>

    <DiagnosticBanner
      v-if="projects.projects.kind === 'error'"
      :code="projects.projects.diagnostic.code"
      :message="projects.projects.diagnostic.message"
      :retryable="projects.projects.diagnostic.retryable"
      severity="error"
      @retry="projects.load"
    />

    <nav class="mode-switch md-card" aria-label="Work 操作">
      <button
        type="button"
        class="mode-switch__button"
        :class="{ 'mode-switch__button--selected': mode === 'configuration' }"
        :aria-pressed="mode === 'configuration'"
        @click="mode = 'configuration'"
      >
        配置现有 Work
      </button>
      <button
        type="button"
        class="mode-switch__button"
        :class="{ 'mode-switch__button--selected': mode === 'template' }"
        :aria-pressed="mode === 'template'"
        @click="mode = 'template'"
      >
        生成 Work 模板
      </button>
    </nav>

    <ProjectAgentRunHistory
      :project-id="selectedProjectId"
      scope="work"
      @open="restoreHistory"
    />

    <WorkTemplateAuthoringView
      v-if="mode === 'template'"
      :key="selectedProjectId ?? 'no-project'"
      :project-id="selectedProject?.id ?? null"
      :course-id="selectedProject?.courseId ?? null"
      :run-id="templateRouteRunId"
      :release-id="templateRouteReleaseId"
      @run-created="persistTemplateRun"
      @release-created="persistTemplateRelease"
    />

    <DiagnosticBanner
      v-if="mode === 'configuration' && agent.outcome"
      :code="agent.outcome.code"
      :message="agent.outcome.message"
      :retryable="agent.outcome.retryable"
      :severity="agent.outcome.code.includes('FAILED') ? 'error' : 'info'"
      @retry="reloadPolicy"
    />

    <div v-if="mode === 'configuration'" class="config-layout">
      <section class="config-card md-card" aria-labelledby="config-heading">
        <h3 id="config-heading">生成 Work 配置</h3>
        <p class="section-note">材料包和策略均由服务端解析。这里提交的是引用和运行时选择，不接受浏览器直接注入镜像或平台权限。</p>

        <AsyncStateView :state="policy" empty-text="当前项目没有已激活的项目 AI 设置。请先完成配置。" @retry="reloadPolicy">
          <template #success="{ data }">
            <div class="policy-summary">
              <div><span>AI 设置</span><span>已启用</span></div>
              <div><span>模型</span><code>{{ data.binding.model }}</code></div>
              <details class="advanced-details">
                <summary>查看 AI 设置详情</summary>
                <small>执行版本：{{ data.binding.claudeCodeVersion }} · 策略版本：{{ data.revision }}</small>
                <small>任务额度：{{ data.budget.maxRequests }} 次调用，输入上限 {{ data.budget.maxInputTokens }} tokens</small>
              </details>
            </div>
          </template>
          <template #empty>
            <div class="policy-missing" data-testid="software-policy-missing">
              <p>当前项目没有已激活的项目 AI 设置。完成配置后才能生成 Work 配置。</p>
              <RouterLink
                class="outlined-button"
                :to="{ path: '/researcher/ai-policy', query: { projectId: selectedProjectId } }"
              >
                打开项目 AI 设置
              </RouterLink>
            </div>
          </template>
        </AsyncStateView>

        <section class="package-upload" aria-labelledby="package-upload-heading">
          <div class="section-heading section-heading--compact">
            <div>
              <h4 id="package-upload-heading">上传软件需求材料</h4>
              <p>选择一个材料文件夹，归档完成后会自动绑定材料包版本。</p>
            </div>
          </div>
          <input
            ref="packageInput"
            type="file"
            webkitdirectory
            directory
            multiple
            class="file-input"
            data-testid="software-package-file-input"
            @change="onPackageInput"
          >
          <button type="button" class="outlined-button" @click="packageInput?.click()">
            选择材料文件夹
          </button>
          <ul v-if="packageUpload.files.length > 0" class="package-file-list" aria-label="待上传材料文件">
            <li v-for="file in packageUpload.files" :key="file.path">
              <span>{{ file.path }}</span>
              <span>{{ packageUpload.formatBytes(file.sizeBytes) }}</span>
              <span>{{ file.status === 'pending' ? '待上传' : file.status === 'uploading' ? `上传中 ${file.progress}%` : file.status === 'done' ? '完成' : '失败' }}</span>
              <button type="button" class="text-button" @click="packageUpload.removeFile(file.path)">移除</button>
            </li>
          </ul>
          <DiagnosticBanner
            v-if="packageUpload.state.kind === 'error'"
            :code="packageUpload.state.diagnostic.code"
            :message="packageUpload.state.diagnostic.message"
            :retryable="packageUpload.state.diagnostic.retryable"
            severity="error"
            @retry="packageUpload.retry"
          />
          <div class="package-upload-actions">
            <button type="button" class="filled-button" :disabled="!canUploadPackage" @click="packageUpload.createSession">
              {{ packageUploadButtonLabel }}
            </button>
            <button v-if="packageDone" type="button" class="text-button" @click="packageUpload.clear">清除材料</button>
          </div>
          <div v-if="packageDone && uploadedPackage" class="package-upload-success" role="status">
            材料包已准备：{{ uploadedPackage.files.length }} 个文件。
            <details class="advanced-details">
              <summary>查看材料版本</summary>
              <small>材料包版本：{{ uploadedPackage.revision }} · 材料包 ID：{{ uploadedPackage.id }}</small>
            </details>
          </div>
        </section>

        <form class="config-form" @submit.prevent="startRun">
          <label>
            <span>Work 环境</span>
            <select v-model="selectedEnvironmentId" class="text-input" :disabled="workEnvironments.environments.kind !== 'success'" required>
              <option value="" disabled>选择现有 Work 环境</option>
              <option v-for="environment in workEnvironments.environments.kind === 'success' ? workEnvironments.environments.data : []" :key="environment.id" :value="environment.id">{{ environment.displayLabel }} · {{ environmentStateLabel(environment.observedState) }}</option>
            </select>
          </label>
          <div v-if="selectedEnvironment" class="readonly-meta">
            <span>Work 环境</span><span>{{ selectedEnvironment.displayLabel }}</span>
            <span>当前状态</span><span>{{ environmentStateLabel(selectedEnvironment.observedState) }}</span>
            <details class="advanced-details">
              <summary>查看环境版本</summary>
              <code>Revision {{ selectedEnvironment.revision }}</code>
            </details>
          </div>
          <label class="authorization-field">
            <input v-model="impactAcknowledged" type="checkbox" />
            <span>我确认 Agent 可能修改该 Work 的配置；如涉及重启或超出预授权范围，服务端会要求明确确认。</span>
          </label>
          <button type="submit" class="filled-button" :disabled="!canStart || agent.acting !== null">
            {{ agent.acting === 'start' ? '提交中…' : '生成 Work 配置' }}
          </button>
        </form>
      </section>

      <section class="run-card md-card" aria-labelledby="run-heading">
        <div class="section-heading">
          <div>
            <h3 id="run-heading">配置任务</h3>
            <p>查看配置任务进度、等待原因和下一步操作。</p>
          </div>
          <button v-if="agent.run.kind === 'success'" type="button" class="icon-button" aria-label="刷新配置任务" @click="agent.load(agent.run.data.id)"><SvgIcon name="refresh" size="sm" aria-hidden="true" /></button>
        </div>
        <AsyncStateView :state="agent.run" empty-text="提交软件需求后，这里会显示任务状态。" @retry="reloadRun">
          <template #success="{ data }">
            <div class="run-overview">
              <div><span>任务</span><span>Work 配置任务</span></div>
              <div><span>状态</span><span class="state-chip" :class="`state-chip--${data.state}`">{{ runStateLabel(data.state) }}</span></div>
              <details class="advanced-details">
                <summary>查看任务标识</summary>
                <small>Run ID：{{ data.id }} · Revision：{{ data.revision }}</small>
              </details>
            </div>
            <div class="track-list">
              <article v-for="track in data.tracks" :key="track.kind" class="track-item">
                <div class="track-heading">
                  <strong>{{ track.kind === 'work_configuration' ? 'Work 配置' : track.kind === 'environment' ? 'Environment 候选' : 'Evaluation 候选' }}</strong>
                  <details v-if="track.candidateId" class="advanced-details">
                    <summary>查看候选标识</summary>
                    <code>{{ track.candidateId }}</code>
                  </details>
                </div>
                <ul>
                  <li v-for="attempt in track.attempts" :key="attempt.number">
                    <span>尝试 {{ attempt.number }}</span>
                    <span>{{ trackStateLabel(attempt.state) }}</span>
                    <details v-if="attempt.diagnosticCode" class="advanced-details">
                      <summary>查看诊断</summary>
                      <code>{{ attempt.diagnosticCode }}</code>
                    </details>
                  </li>
                </ul>
                <button v-if="track.kind === 'work_configuration' && trackCanRetry(data, 'work_configuration') && !data.plan" type="button" class="text-button" :disabled="agent.acting !== null" @click="agent.retryTrack('work_configuration')">重试 Work 配置</button>
                <button v-if="track.kind === 'environment' && trackCanRetry(data, 'environment')" type="button" class="text-button" :disabled="agent.acting !== null" @click="agent.retryTrack('environment')">重试 Environment</button>
              </article>
            </div>
            <section v-if="workConfigurationNeedsNewTask(data)" class="work-plan-retry-hint" aria-label="创建新的 Work 配置任务">
              <strong>此 Run 已经生成 Work 配置计划，不能在原 Run 上普通重试。</strong>
              <p>计划是不可变提案；请提交新的材料包、Work 环境和授权说明来创建新的 Work 配置任务。</p>
              <button type="button" class="outlined-button" @click="prepareNewTask">开始新的 Work 配置任务</button>
            </section>
            <div class="run-actions">
              <button v-if="data.state === 'requested' || data.state === 'running' || data.state === 'awaiting_approval'" type="button" class="outlined-button danger-button" :disabled="agent.acting !== null" @click="agent.cancel">取消配置任务</button>
            </div>
          </template>
        </AsyncStateView>

        <AsyncStateView v-if="plan.kind !== 'idle'" :state="plan" empty-text="当前 Run 尚未生成 Work 配置计划。" @retry="reloadPlan">
          <template #success="{ data }">
            <section class="plan-section" aria-labelledby="plan-heading">
              <div class="section-heading">
                <div>
                  <h4 id="plan-heading">Work 配置计划审核</h4>
                  <p>查看完整脚本、目标环境和重启影响后，再提交批准。</p>
                </div>
                <span class="state-chip state-chip--awaiting_approval">等待批准</span>
              </div>
              <div class="plan-meta">
                <div><span>目标环境</span><span>{{ selectedEnvironment?.displayLabel ?? '当前 Work 环境' }}</span></div>
                <div><span>重启影响</span><span>{{ data.plan.requiresRestart ? '需要重启' : '无需重启' }}</span></div>
                <div><span>变更说明</span><span>{{ data.plan.summary }}</span></div>
                <details class="advanced-details">
                  <summary>查看计划标识</summary>
                  <small>环境 ID：{{ data.plan.environmentId }} · 环境版本：{{ data.plan.environmentRevision }} · 计划 ID：{{ data.plan.id }} / 版本：{{ data.plan.revision }}</small>
                </details>
              </div>
              <p v-if="data.plan.requiresRestart" class="restart-warning" role="alert">此计划需要重启 Work 环境，执行期间连接会暂时中断。</p>
              <p v-else class="section-note">此计划不需要重启 Work 环境。</p>
              <div class="plan-code">
                <div>
                  <h5>配置脚本</h5>
                  <pre>{{ data.scriptContent }}</pre>
                </div>
                <div v-if="data.verificationScriptContent">
                  <h5>验证脚本</h5>
                  <pre>{{ data.verificationScriptContent }}</pre>
                </div>
              </div>
              <DiagnosticBanner
                v-if="planApprovalOutcome"
                :code="planApprovalOutcome.code"
                :message="planApprovalOutcome.message"
                :retryable="planApprovalOutcome.retryable"
                :severity="planApprovalOutcome.code.includes('FAILED') ? 'error' : 'info'"
                @retry="reloadPlan"
              />
              <form v-if="agent.run.kind === 'success' && agent.run.data.state === 'awaiting_approval'" class="approval-form" @submit.prevent="approvePlan">
                <label>
                  <span>批准原因</span>
                  <textarea v-model="approvalReason" class="text-input" rows="3" required placeholder="说明批准这次 Work 配置的原因" />
                </label>
                <label v-if="data.plan.requiresRestart" class="authorization-field">
                  <input v-model="restartConfirmed" type="checkbox" />
                  <span>我确认执行前后 Work 环境会重启，现有连接可能暂时中断。</span>
                </label>
                <label>
                  <span>批准有效期</span>
                  <input v-model="approvalExpiresAt" class="text-input" type="datetime-local" required />
                </label>
                <button type="submit" class="filled-button" :disabled="!canApprovePlan || approving">
                  {{ approving ? '提交批准中…' : '批准并执行 Work 配置' }}
                </button>
              </form>
            </section>
          </template>
        </AsyncStateView>
      </section>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, inject, ref, watch } from 'vue'
import { RouterLink, routeLocationKey, routerKey } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import ProjectAgentRunHistory from '@/components/common/ProjectAgentRunHistory.vue'
import WorkTemplateAuthoringView from '@/views/researcher/WorkTemplateAuthoringView.vue'
import { approveProjectWorkConfigurationRun, getActiveProjectLlmPolicy, getProjectWorkConfigurationPlan } from '@/generated/contracts'
import type { AgentRunHistoryItem, AgentRunSchema, ProblemPackageSchema, ProjectLlmEgressPolicySchema, WorkConfigurationPlanViewSchema } from '@/generated/contracts'
import { useProjectAgentRun } from '@/composables/useProjectAgentRun'
import { useProjectProblemPackageUpload } from '@/composables/useProjectProblemPackageUpload'
import { useProjectWorkEnvironments } from '@/composables/useProjectWorkEnvironments'
import { useProjects } from '@/composables/useProjects'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { environmentStateLabel } from '@/utils/stateLabels'
import { idempotencyKey, ifMatch } from '@/utils/format'

const projects = useProjects()
const route = inject(routeLocationKey, null)
const router = inject(routerKey, null)
const routeProjectId = computed(() => {
  const id = typeof route?.query.projectId === 'string' ? route.query.projectId.trim() : ''
  return id || null
})
const routePackageId = computed(() => {
  const id = typeof route?.query.packageId === 'string' ? route.query.packageId.trim() : ''
  return id || null
})
const routeRunId = computed(() => {
  const id = typeof route?.query.runId === 'string' ? route.query.runId.trim() : ''
  return id || null
})
const routeReleaseId = computed(() => {
  const id = typeof route?.query.releaseId === 'string' ? route.query.releaseId.trim() : ''
  return id || null
})
const routeMode = computed<'configuration' | 'template'>(() => route?.query.mode === 'template' ? 'template' : 'configuration')
const mode = ref<'configuration' | 'template'>(routeMode.value)
const selectedProjectId = ref<string | null>(routeProjectId.value)
const selectedProject = computed(() => projects.projects.kind === 'success' ? projects.projects.data.find((project) => project.id === selectedProjectId.value) ?? null : null)
const projectOptions = computed(() => projects.projects.kind === 'success' ? projects.projects.data : [])
const projectIdRef = computed(() => selectedProjectId.value)
const selectedEnvironmentId = ref('')
const agent = useProjectAgentRun(projectIdRef, { kind: 'work_configuration' })
const routeRunMatchesProject = computed(() => Boolean(routeProjectId.value && routeProjectId.value === selectedProjectId.value))
const configurationRouteRunId = computed(() => (
  mode.value === 'configuration' && routeMode.value === 'configuration' && routeRunMatchesProject.value
    ? routeRunId.value
    : null
))
const templateRouteRunId = computed(() => (
  mode.value === 'template' && routeMode.value === 'template' && routeRunMatchesProject.value
    ? routeRunId.value
    : null
))
const templateRouteReleaseId = computed(() => (
  mode.value === 'template' && routeMode.value === 'template' && routeRunMatchesProject.value
    ? routeReleaseId.value
    : null
))

const policy = ref<AsyncState<ProjectLlmEgressPolicySchema>>({ kind: 'idle' })
const policyRevision = computed(() => policy.value.kind === 'success' ? policy.value.data.revision : undefined)
const courseIdRef = computed(() => selectedProject.value?.courseId ?? null)
const packageUpload = useProjectProblemPackageUpload(projectIdRef, policyRevision, courseIdRef)
const workEnvironments = useProjectWorkEnvironments(projectIdRef)
const selectedEnvironment = computed(() => workEnvironments.environments.kind === 'success' ? workEnvironments.environments.data.find((environment) => environment.id === selectedEnvironmentId.value) ?? null : null)
const plan = ref<AsyncState<WorkConfigurationPlanViewSchema>>({ kind: 'idle' })
const planApprovalOutcome = ref<DiagnosticViewModel | null>(null)
const packageInput = ref<HTMLInputElement | null>(null)
const uploadedPackage = computed<ProblemPackageSchema | null>(() => packageUpload.state.kind === 'done' ? packageUpload.state.package : null)
const packageDone = computed(() => uploadedPackage.value !== null)
const packageId = computed(() => uploadedPackage.value?.id ?? '')
const packageRevision = computed(() => uploadedPackage.value?.revision ?? 0)
let packageRestoreKey: string | null = null
let packageRestoreGeneration = 0
const canUploadPackage = computed(() => {
  const ready = packageUpload.state.kind === 'ready' || packageUpload.state.kind === 'error'
  return Boolean(selectedProject.value && ready && packageUpload.files.length > 0 && policyRevision.value !== undefined)
})
const packageUploadButtonLabel = computed(() => {
  switch (packageUpload.state.kind) {
    case 'creating': return '创建上传会话…'
    case 'uploading': return '上传中…'
    case 'completing': return '确认归档…'
    case 'loading': return '读取已归档材料包…'
    default: return '上传材料包'
  }
})
const impactAcknowledged = ref(false)
const approvalReason = ref('')
const restartConfirmed = ref(false)
const approvalExpiresAt = ref('')
const approving = ref(false)
const existingRunForCurrentInputs = computed(() => {
  if (agent.run.kind !== 'success' || policy.value.kind !== 'success' || !selectedEnvironment.value || !packageId.value.trim()) return false
  const current = agent.run.data
  return current.projectId === selectedProjectId.value
    && current.packageId === packageId.value.trim()
    && current.policyId === policy.value.data.id
    && current.policyRevision === policy.value.data.revision
    && current.purpose.kind === 'work_configuration'
    && current.purpose.environmentId === selectedEnvironment.value.id
    && current.purpose.environmentRevision === selectedEnvironment.value.revision
})
const canStart = computed(() => Boolean(selectedProject.value && packageId.value.trim() && Number.isInteger(packageRevision.value) && packageRevision.value > 0 && selectedEnvironment.value && impactAcknowledged.value && policy.value.kind === 'success' && agent.run.kind !== 'loading' && !existingRunForCurrentInputs.value))
const canApprovePlan = computed(() => {
  if (plan.value.kind !== 'success' || agent.run.kind !== 'success' || agent.run.data.state !== 'awaiting_approval') return false
  if (!approvalReason.value.trim() || !approvalExpiresAt.value) return false
  if (plan.value.data.plan.requiresRestart && !restartConfirmed.value) return false
  return new Date(approvalExpiresAt.value).getTime() > Date.now()
})

watch(
  () => projects.projects,
  (state) => {
    // Keep an explicit route project while the shared project list is still
    // loading. Clearing it here makes the first mode switch rewrite the URL
    // without projectId, so the template view cannot bind its upload to the
    // project that the user just opened.
    if (state.kind === 'idle' || state.kind === 'loading' || state.kind === 'error') return
    const items = state.kind === 'success' ? state.data : []
    const preferred = routeProjectId.value
    const next = preferred && items.some((project) => project.id === preferred)
      ? preferred
      : selectedProjectId.value && items.some((project) => project.id === selectedProjectId.value)
        ? selectedProjectId.value
        : items[0]?.id ?? null
    if (selectedProjectId.value !== next) selectedProjectId.value = next
    if (next && projects.selectedProjectId !== next) projects.select(next)
  },
  { immediate: true },
)

watch(routeProjectId, (id) => {
  if (!id || !projectOptions.value.some((project) => project.id === id)) return
  if (selectedProjectId.value !== id) selectedProjectId.value = id
  if (projects.selectedProjectId !== id) projects.select(id)
})

watch(routeMode, (nextMode) => {
  if (mode.value !== nextMode) mode.value = nextMode
}, { immediate: true })

let planRunId: string | null = null
let configurationRouteKey: string | null = null
let policyGeneration = 0
let planGeneration = 0
let approvalOperationGeneration = 0
function restoreHistory(item: AgentRunHistoryItem) {
  if (!router || !selectedProjectId.value) return
  const historyMode = item.purpose.kind === 'authoring' && item.purpose.environmentClass === 'work' ? 'template' : 'configuration'
  void router.replace({
    query: {
      ...route?.query,
      projectId: selectedProjectId.value,
      mode: historyMode === 'template' ? 'template' : undefined,
      runId: item.id,
      releaseId: undefined,
    },
  })
}

function invalidatePlan() {
  planGeneration += 1
  planRunId = null
  plan.value = { kind: 'idle' }
  planApprovalOutcome.value = null
}

function syncSoftwareRoute(
  runId: string | null | undefined = routeRunId.value,
  releaseId: string | null | undefined = routeReleaseId.value,
  packageIdValue: string | null | undefined = routePackageId.value,
) {
  if (!router || !route) return
  void router.replace({
    query: {
      ...route.query,
      projectId: selectedProjectId.value
        ?? ((projects.projects.kind === 'idle' || projects.projects.kind === 'loading')
          ? routeProjectId.value
          : undefined),
      mode: mode.value === 'template' ? 'template' : undefined,
      runId: runId ?? undefined,
      releaseId: releaseId ?? undefined,
      packageId: packageIdValue ?? undefined,
    },
  })
}

watch(selectedProjectId, (id, previousId) => {
  selectedEnvironmentId.value = ''
  invalidatePlan()
  approvalOperationGeneration += 1
  approving.value = false
  approvalReason.value = ''
  restartConfirmed.value = false
  approvalExpiresAt.value = defaultApprovalExpiry()
  const sameProject = routeProjectId.value === id
  syncSoftwareRoute(
    previousId && previousId !== id ? null : sameProject ? routeRunId.value : null,
    previousId && previousId !== id ? null : sameProject ? routeReleaseId.value : null,
    previousId && previousId !== id ? null : sameProject ? routePackageId.value : null,
  )
  policyGeneration += 1
  void reloadPolicy()
}, { immediate: true })

watch(mode, (nextMode, previousMode) => {
  if (nextMode === previousMode) return
  const routeRestoresNextMode = routeMode.value === nextMode
    && routeRunId.value !== null
    && routeProjectId.value === selectedProjectId.value
  configurationRouteKey = null
  invalidatePlan()
  approvalOperationGeneration += 1
  approving.value = false
  policyGeneration += 1
  agent.reset()
  policy.value = { kind: 'idle' }
  if (!routeRestoresNextMode) syncSoftwareRoute(null, null)
  if (nextMode === 'configuration') void reloadPolicy()
})

async function restorePackageContext() {
  const projectId = selectedProjectId.value
  const packageId = routePackageId.value
  const contextKey = `${projectId ?? ''}:${packageId ?? ''}`
  if (contextKey === packageRestoreKey) return
  packageRestoreKey = contextKey
  const generation = ++packageRestoreGeneration
  if (mode.value !== 'configuration' || !projectId || !packageId) return
  await packageUpload.loadPackage(packageId)
  if (generation !== packageRestoreGeneration || selectedProjectId.value !== projectId || routePackageId.value !== packageId) return
}

watch([selectedProjectId, routePackageId, mode], () => {
  void restorePackageContext()
}, { immediate: true })

function persistTemplateRun(runId: string) {
  syncSoftwareRoute(runId, null)
}

function persistTemplateRelease(releaseId: string) {
  syncSoftwareRoute(templateRouteRunId.value, releaseId)
}

function configurationEnvironmentDiagnostic(data: AgentRunSchema): DiagnosticViewModel | undefined {
  if (data.purpose.kind !== 'work_configuration') return undefined
  const environments = workEnvironments.environments
  if (environments.kind === 'empty') {
    return makeDiagnostic('PROJECT_RUN_ENVIRONMENT_MISMATCH', '当前任务绑定的 Work 环境已不在当前项目中，已停止恢复。', false)
  }
  if (environments.kind !== 'success') return undefined
  const environmentId = data.purpose.kind === 'work_configuration' ? data.purpose.environmentId : null
  const target = environments.data.find((environment) => environment.id === environmentId && environment.projectId === selectedProjectId.value)
  return target
    ? undefined
    : makeDiagnostic('PROJECT_RUN_ENVIRONMENT_MISMATCH', '当前任务绑定的 Work 环境不在当前项目中，已停止恢复。', false)
}

type ConfigurationEnvironmentStatus = 'valid' | 'pending' | 'invalid'

function reconcileConfigurationEnvironment(state: AsyncState<AgentRunSchema>): ConfigurationEnvironmentStatus {
  if (mode.value !== 'configuration' || state.kind !== 'success' || state.data.purpose.kind !== 'work_configuration') return 'valid'
  const diagnostic = configurationEnvironmentDiagnostic(state.data)
  if (diagnostic) {
    agent.invalidate(diagnostic)
    return 'invalid'
  }
  if (workEnvironments.environments.kind !== 'success') return 'pending'
  if (workEnvironments.environments.kind === 'success') selectedEnvironmentId.value = state.data.purpose.environmentId
  return 'valid'
}

function syncPlanToCurrentRun() {
  const state = agent.run
  if (state.kind !== 'success' || state.data.state !== 'awaiting_approval') {
    invalidatePlan()
    return
  }
  const environmentStatus = reconcileConfigurationEnvironment(state)
  if (environmentStatus !== 'valid') {
    invalidatePlan()
    return
  }
  if (planRunId !== state.data.id) {
    planRunId = state.data.id
    void reloadPlan(state.data.id)
  }
}

watch(
  [mode, selectedProjectId, configurationRouteRunId],
  ([nextMode, projectId, runId]) => {
    const nextKey = nextMode === 'configuration' && projectId && runId ? `${projectId}:${runId}` : null
    if (nextKey === configurationRouteKey) return
    configurationRouteKey = nextKey
    invalidatePlan()
    if (!nextKey || !runId) {
      agent.reset()
      return
    }
    void agent.load(runId)
  },
  { immediate: true },
)

watch(
  () => workEnvironments.environments,
  (state) => {
    if (state.kind === 'success') {
      const restoredRun = agent.run.kind === 'success' ? agent.run.data : null
      const restoredEnvironmentId = restoredRun?.purpose.kind === 'work_configuration' ? restoredRun.purpose.environmentId : null
      if (restoredEnvironmentId && state.data.some((environment) => environment.id === restoredEnvironmentId)) selectedEnvironmentId.value = restoredEnvironmentId
      else if (!state.data.some((environment) => environment.id === selectedEnvironmentId.value)) selectedEnvironmentId.value = state.data[0]?.id ?? ''
    } else if (state.kind === 'empty') {
      selectedEnvironmentId.value = ''
    }
    if (reconcileConfigurationEnvironment(agent.run) === 'invalid') invalidatePlan()
    syncPlanToCurrentRun()
  },
  { immediate: true },
)

watch(
  () => agent.run,
  (state) => {
    if (reconcileConfigurationEnvironment(state) === 'invalid') invalidatePlan()
    syncPlanToCurrentRun()
  },
)

async function reloadPolicy() {
  const id = selectedProjectId.value
  const generation = ++policyGeneration
  const requestedMode = mode.value
  if (!id) {
    if (generation === policyGeneration) policy.value = { kind: 'idle' }
    return
  }
  policy.value = { kind: 'loading', message: '加载项目 LLM 策略…' }
  const result = await getActiveProjectLlmPolicy({ path: { projectId: id } })
  if (generation !== policyGeneration || selectedProjectId.value !== id || mode.value !== requestedMode) return
  if (result.error) {
    const problem = extractProblemDetails(result.error)
    policy.value = { kind: 'error', diagnostic: makeDiagnostic(problem?.diagnosticCode ?? 'PROJECT_POLICY_LOAD_FAILED', problem?.detail ?? '加载项目 LLM 策略失败', problem?.retryable ?? true) }
    return
  }
  policy.value = { kind: 'success', data: result.data }
}

async function startRun() {
  if (!canStart.value || policy.value.kind !== 'success') return
  const environment = selectedEnvironment.value
  if (!environment) return
  const started = await agent.startWorkConfiguration({
    environmentId: environment.id,
    environmentRevision: environment.revision,
    packageId: packageId.value.trim(),
    packageRevision: packageRevision.value,
    policyId: policy.value.data.id,
    policyRevision: policy.value.data.revision,
    ...(selectedProject.value?.courseId ? { courseId: selectedProject.value.courseId } : {}),
  })
  if (started && agent.run.kind === 'success' && agent.run.data.purpose.kind === 'work_configuration') {
    configurationRouteKey = selectedProjectId.value ? `${selectedProjectId.value}:${agent.run.data.id}` : null
    syncSoftwareRoute(agent.run.data.id, null)
  }
}

async function reloadPlan(runId?: string) {
  const id = selectedProjectId.value
  const resolvedRunId = runId ?? (agent.run.kind === 'success' && agent.run.data.state === 'awaiting_approval' ? agent.run.data.id : undefined)
  if (!id || !resolvedRunId) {
    invalidatePlan()
    return
  }
  const generation = ++planGeneration
  const requestedMode = mode.value
  plan.value = { kind: 'loading', message: '加载 Work 配置计划…' }
  const result = await getProjectWorkConfigurationPlan({ path: { projectId: id, runId: resolvedRunId } })
  const currentRun = agent.run.kind === 'success' ? agent.run.data : null
  if (
    generation !== planGeneration
    || selectedProjectId.value !== id
    || mode.value !== requestedMode
    || !currentRun
    || currentRun.id !== resolvedRunId
    || currentRun.state !== 'awaiting_approval'
  ) return
  if (result.error) {
    const problem = extractProblemDetails(result.error)
    plan.value = { kind: 'error', diagnostic: makeDiagnostic(problem?.diagnosticCode ?? 'PROJECT_WORK_PLAN_LOAD_FAILED', problem?.detail ?? '加载 Work 配置计划失败', problem?.retryable ?? true) }
    return
  }
  plan.value = { kind: 'success', data: result.data }
  if (!approvalExpiresAt.value) approvalExpiresAt.value = defaultApprovalExpiry()
}

async function approvePlan() {
  if (!canApprovePlan.value || plan.value.kind !== 'success' || agent.run.kind !== 'success' || !selectedProjectId.value) return
  const projectId = selectedProjectId.value
  const requestedMode = mode.value
  const currentRun = agent.run.data
  const currentRunId = currentRun.id
  const operationGeneration = ++approvalOperationGeneration
  approving.value = true
  planApprovalOutcome.value = null
  const currentPlan = plan.value.data.plan
  try {
    const result = await approveProjectWorkConfigurationRun({
      path: { projectId, runId: currentRunId },
      headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(currentRun.revision) },
      body: {
        expectedRunRevision: currentRun.revision,
        expectedPlanRevision: currentPlan.revision,
        environmentRevision: currentPlan.environmentRevision,
        expiresAt: new Date(approvalExpiresAt.value).toISOString(),
        reason: approvalReason.value.trim(),
        restartConfirmed: restartConfirmed.value,
      },
    })
    const current = () => operationGeneration === approvalOperationGeneration
      && selectedProjectId.value === projectId
      && mode.value === requestedMode
      && agent.run.kind === 'success'
      && agent.run.data.id === currentRunId
    if (!current()) return
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      planApprovalOutcome.value = makeDiagnostic(problem?.diagnosticCode ?? 'PROJECT_WORK_PLAN_APPROVAL_FAILED', problem?.detail ?? '批准 Work 配置失败', problem?.retryable ?? true)
      return
    }
    if (!result.data) {
      planApprovalOutcome.value = makeDiagnostic('PROJECT_WORK_PLAN_APPROVAL_FAILED', '批准 Work 配置没有返回 AgentRun。', true)
      return
    }
    planApprovalOutcome.value = makeDiagnostic('PROJECT_WORK_PLAN_APPROVED', 'Work 配置已批准，正在执行。', false)
    approvalReason.value = ''
    restartConfirmed.value = false
    invalidatePlan()
    await agent.load(result.data.id)
  } finally {
    if (operationGeneration === approvalOperationGeneration) approving.value = false
  }
}

function defaultApprovalExpiry() {
  const expiry = new Date(Date.now() + 15 * 60 * 1000)
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${expiry.getFullYear()}-${pad(expiry.getMonth() + 1)}-${pad(expiry.getDate())}T${pad(expiry.getHours())}:${pad(expiry.getMinutes())}`
}

function reloadRun() {
  if (configurationRouteRunId.value) return agent.load(configurationRouteRunId.value)
  if (agent.run.kind === 'success') return agent.load(agent.run.data.id)
  return undefined
}

function workConfigurationNeedsNewTask(data: AgentRunSchema) {
  return data.purpose.kind === 'work_configuration'
    && data.plan !== null
    && data.plan !== undefined
    && (data.state === 'failed' || data.state === 'partially_succeeded' || data.state === 'cancelled')
}

function onPackageInput(event: Event) {
  const target = event.target as HTMLInputElement
  if (target.files && target.files.length > 0) packageUpload.addFiles(Array.from(target.files))
  target.value = ''
}

function trackCanRetry(data: AgentRunSchema, kind: AgentRunSchema['tracks'][number]['kind']): boolean {
  if (data.state !== 'failed' && data.state !== 'partially_succeeded' && data.state !== 'cancelled') return false
  const track = data.tracks.find((item) => item.kind === kind)
  const latestAttempt = track?.attempts[track.attempts.length - 1]
  return latestAttempt?.state === 'failed' || latestAttempt?.state === 'cancelled'
}

function prepareNewTask() {
  configurationRouteKey = null
  invalidatePlan()
  approvalOperationGeneration += 1
  approving.value = false
  syncSoftwareRoute(null, null, null)
  agent.reset()
  packageUpload.clear()
  selectedEnvironmentId.value = ''
  impactAcknowledged.value = false
  plan.value = { kind: 'idle' }
  planApprovalOutcome.value = null
  packageInput.value?.focus()
}

function runStateLabel(state: string) {
  return ({ requested: '已提交', running: '运行中', awaiting_approval: '等待审批', partially_succeeded: '部分完成', succeeded: '已完成', failed: '失败', cancelling: '取消中', cancelled: '已取消' } as Record<string, string>)[state] ?? state
}

function trackStateLabel(state: string) {
  return ({ pending: '等待处理', requested: '已提交', running: '运行中', succeeded: '已完成', failed: '失败', cancelled: '已取消', skipped: '已跳过' } as Record<string, string>)[state] ?? state
}

</script>

<style scoped>
.software-page { display: grid; gap: 20px; }
.page-header, .section-heading { display: flex; justify-content: space-between; align-items: flex-start; gap: 16px; }
.mode-switch { display: inline-flex; gap: 4px; width: fit-content; padding: 4px; }
.mode-switch__button { min-height: 40px; padding: 0 17px; border: 0; border-radius: var(--md-sys-shape-full); background: transparent; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-large); cursor: pointer; }
.mode-switch__button--selected { background: var(--md-sys-color-primary-container); color: var(--md-sys-color-on-primary-container); }
.page-header h2, .section-heading h3, .plan-section h4 { margin: 0; color: var(--md-sys-color-on-surface); }
.page-header h2 { font: var(--md-sys-headline-small); }
.section-heading h3 { font: var(--md-sys-title-large); }
.plan-section h4 { font: var(--md-sys-title-medium); }
.page-subtitle, .section-note, .section-heading p { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.project-strip { display: flex; align-items: end; gap: 18px; padding: 16px 20px; }
.project-strip label, .config-form label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.project-strip label { flex: 1; max-width: 560px; }
.project-summary { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; padding-bottom: 9px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.project-summary .advanced-details { flex-basis: 100%; }
.config-layout { display: grid; grid-template-columns: minmax(300px, .9fr) minmax(0, 1.3fr); gap: 20px; align-items: start; }
.config-card, .run-card { padding: 20px; }
.policy-missing { display: grid; justify-items: start; gap: 10px; margin: 18px 0; padding: 13px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); color: var(--md-sys-color-on-surface-variant); }
.policy-missing p { margin: 0; }
.package-upload { display: grid; gap: 12px; margin-top: 20px; padding-top: 18px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.package-upload h4 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.file-input { position: absolute; width: 1px; height: 1px; overflow: hidden; opacity: 0; pointer-events: none; }
.package-file-list { display: grid; gap: 6px; margin: 0; padding: 0; list-style: none; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.package-file-list li { display: grid; grid-template-columns: minmax(0, 1fr) auto auto auto; align-items: center; gap: 10px; padding: 8px 10px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.package-file-list li > span:first-child { overflow-wrap: anywhere; color: var(--md-sys-color-on-surface); }
.package-upload-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }
.package-upload-success { margin: 0; color: var(--md-sys-color-tertiary); font: var(--md-sys-body-small); }
.package-upload-success .advanced-details { margin-top: 6px; }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.policy-summary, .run-overview, .plan-meta { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px 14px; margin: 18px 0; padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); font: var(--md-sys-body-small); }
.policy-summary span, .run-overview span, .plan-meta > div > span:first-child { color: var(--md-sys-color-on-surface-variant); }
.policy-summary .advanced-details, .run-overview .advanced-details, .plan-meta > .advanced-details { grid-column: 1 / -1; }
.policy-summary code, .run-overview code, .plan-meta code { overflow-wrap: anywhere; color: var(--md-sys-color-on-surface); }
.config-form { display: grid; gap: 14px; margin-top: 20px; }
.authorization-field { display: flex !important; grid-template-columns: auto 1fr; align-items: flex-start; gap: 9px !important; line-height: 1.45; }
.authorization-field input { margin-top: 3px; }
.filled-button, .outlined-button, .text-button { display: inline-flex; justify-content: center; align-items: center; gap: 8px; min-height: 40px; padding: 0 17px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-large); cursor: pointer; text-decoration: none; }
.filled-button { border: 1px solid var(--md-sys-color-primary); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); }
.outlined-button { border: 1px solid var(--md-sys-color-outline); background: transparent; color: var(--md-sys-color-primary); }
.text-button { min-height: 32px; border: 0; background: transparent; color: var(--md-sys-color-primary); }
.danger-button { color: var(--md-sys-color-error); border-color: var(--md-sys-color-error); }
.filled-button:disabled, .outlined-button:disabled, .text-button:disabled { opacity: .5; cursor: not-allowed; }
.state-chip { display: inline-flex; width: fit-content; padding: 3px 8px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.state-chip--active, .state-chip--succeeded { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--failed, .state-chip--cancelled { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
.state-chip--archived { background: var(--md-sys-color-surface-variant); }
.run-card { display: grid; gap: 15px; }
.work-plan-retry-hint { display: grid; gap: 8px; margin-top: 2px; padding: 14px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); background: var(--md-sys-color-surface-container-low); }
.work-plan-retry-hint strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.work-plan-retry-hint p { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
.track-list { display: grid; gap: 10px; }
.track-item { padding: 14px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.track-heading, .track-item li, .run-actions { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
.track-heading code, .track-item code { color: var(--md-sys-color-on-surface-variant); overflow-wrap: anywhere; }
.track-item ul { display: grid; gap: 5px; margin: 10px 0; padding: 0; list-style: none; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.plan-section { margin-top: 4px; padding-top: 18px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.plan-meta { grid-template-columns: auto minmax(0, 1fr); }
.plan-meta > div { display: contents; }
.plan-meta > div > span:last-child { overflow-wrap: anywhere; }
.plan-code { display: grid; gap: 14px; margin-top: 14px; }
.plan-code h5 { margin: 0 0 6px; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.plan-code pre { max-height: 320px; overflow: auto; margin: 0; padding: 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-small); white-space: pre-wrap; overflow-wrap: anywhere; }
.restart-warning { margin: 14px 0; padding: 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); font: var(--md-sys-body-medium); }
.approval-form { display: grid; gap: 14px; margin-top: 18px; padding-top: 18px; border-top: 1px solid var(--md-sys-color-outline-variant); }
@media (max-width: 820px) { .config-layout { grid-template-columns: 1fr; } .project-strip { align-items: stretch; flex-direction: column; } .project-strip label { max-width: none; } .project-summary { padding-bottom: 0; } }
</style>
