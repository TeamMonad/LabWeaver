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
          <option v-for="project in projectOptions" :key="project.id" :value="project.id">{{ project.name }} · {{ project.id }}</option>
        </select>
      </label>
      <div v-if="selectedProject" class="project-summary">
        <span class="state-chip" :class="`state-chip--${selectedProject.state}`">{{ selectedProject.state === 'active' ? '运行中' : '已归档' }}</span>
        <span>{{ selectedProject.courseId ? `课程 ${selectedProject.courseId}` : '独立科研项目' }}</span>
      </div>
    </section>

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

    <WorkTemplateAuthoringView
      v-if="mode === 'template'"
      :key="selectedProjectId ?? 'no-project'"
      :project-id="selectedProjectId"
      :course-id="selectedProject?.courseId ?? null"
      :run-id="routeRunId"
      :release-id="routeReleaseId"
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

        <AsyncStateView :state="policy" empty-text="当前项目没有已激活的 LLM 策略。请先由管理员配置。" @retry="reloadPolicy">
          <template #success="{ data }">
            <div class="policy-summary">
              <div><span>模型</span><code>{{ data.binding.model }}</code></div>
              <div><span>Claude Code</span><code>{{ data.binding.claudeCodeVersion }}</code></div>
              <div><span>策略</span><code>rev-{{ data.revision }}</code></div>
              <div><span>预算</span><code>{{ data.budget.maxRequests }} requests / {{ data.budget.maxInputTokens }} input tokens</code></div>
            </div>
          </template>
        </AsyncStateView>

        <form class="config-form" @submit.prevent="startRun">
          <label>
            <span>材料包 ID</span>
            <input ref="packageInput" v-model="packageId" class="text-input" required placeholder="已归档 ProblemPackage ID" />
          </label>
          <label>
            <span>材料包 Revision</span>
            <input v-model.number="packageRevision" class="text-input" type="number" min="1" required />
          </label>
          <label>
            <span>Work 环境</span>
            <select v-model="selectedEnvironmentId" class="text-input" :disabled="workEnvironments.environments.kind !== 'success'" required>
              <option value="" disabled>选择现有 Work 环境</option>
              <option v-for="environment in workEnvironments.environments.kind === 'success' ? workEnvironments.environments.data : []" :key="environment.id" :value="environment.id">{{ environment.displayLabel }} · rev-{{ environment.revision }}</option>
            </select>
          </label>
          <div v-if="selectedEnvironment" class="readonly-meta">
            <span>Work 环境 Revision</span><code>rev-{{ selectedEnvironment.revision }}</code>
            <span>当前状态</span><span>{{ selectedEnvironment.observedState }}</span>
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
            <h3 id="run-heading">AgentRun</h3>
            <p>运行和候选状态由 Agent / Control 返回。</p>
          </div>
          <button v-if="agent.run.kind === 'success'" type="button" class="icon-button" aria-label="刷新 AgentRun" @click="agent.load(agent.run.data.id)"><SvgIcon name="refresh" size="sm" aria-hidden="true" /></button>
        </div>
        <AsyncStateView :state="agent.run" empty-text="提交软件需求后，这里会显示 AgentRun。" @retry="reloadRun">
          <template #success="{ data }">
            <div class="run-overview">
              <div><span>Run</span><code>{{ data.id }}</code></div>
              <div><span>状态</span><span class="state-chip" :class="`state-chip--${data.state}`">{{ runStateLabel(data.state) }}</span></div>
              <div><span>Revision</span><code>rev-{{ data.revision }}</code></div>
            </div>
            <div class="track-list">
              <article v-for="track in data.tracks" :key="track.kind" class="track-item">
                <div class="track-heading">
                  <strong>{{ track.kind === 'work_configuration' ? 'Work 配置' : track.kind === 'environment' ? 'Environment 候选' : 'Evaluation 候选' }}</strong>
                  <code v-if="track.candidateId">{{ track.candidateId }}</code>
                </div>
                <ul>
                  <li v-for="attempt in track.attempts" :key="attempt.number">
                    <span>尝试 {{ attempt.number }}</span>
                    <span>{{ attempt.state }}</span>
                    <code v-if="attempt.diagnosticCode">{{ attempt.diagnosticCode }}</code>
                  </li>
                </ul>
                <button v-if="(data.state === 'failed' || data.state === 'partially_succeeded') && track.kind === 'work_configuration' && !data.plan" type="button" class="text-button" :disabled="agent.acting !== null" @click="agent.retryTrack('work_configuration')">重试 Work 配置</button>
                <button v-if="(data.state === 'failed' || data.state === 'partially_succeeded') && track.kind === 'environment'" type="button" class="text-button" :disabled="agent.acting !== null" @click="agent.retryTrack('environment')">重试 Environment</button>
              </article>
            </div>
            <section v-if="workConfigurationNeedsNewTask(data)" class="work-plan-retry-hint" aria-label="创建新的 Work 配置任务">
              <strong>此 Run 已经生成 Work 配置计划，不能在原 Run 上普通重试。</strong>
              <p>计划是不可变提案；请提交新的材料包、Work 环境和授权说明来创建新的 Work 配置任务。</p>
              <button type="button" class="outlined-button" @click="prepareNewTask">开始新的 Work 配置任务</button>
            </section>
            <div class="run-actions">
              <button v-if="data.state === 'requested' || data.state === 'running' || data.state === 'awaiting_approval'" type="button" class="outlined-button danger-button" :disabled="agent.acting !== null" @click="agent.cancel">取消 AgentRun</button>
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
                <div><span>Target Work</span><code>{{ data.plan.environmentId }}</code></div>
                <div><span>Environment Revision</span><code>rev-{{ data.plan.environmentRevision }}</code></div>
                <div><span>Plan</span><code>{{ data.plan.id }} / rev-{{ data.plan.revision }}</code></div>
                <div><span>Summary</span><span>{{ data.plan.summary }}</span></div>
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
import WorkTemplateAuthoringView from '@/views/researcher/WorkTemplateAuthoringView.vue'
import { approveProjectWorkConfigurationRun, getActiveProjectLlmPolicy, getProjectWorkConfigurationPlan } from '@/generated/contracts'
import type { AgentRunSchema, ProjectLlmEgressPolicySchema, WorkConfigurationPlanViewSchema } from '@/generated/contracts'
import { useProjectAgentRun } from '@/composables/useProjectAgentRun'
import { useProjectWorkEnvironments } from '@/composables/useProjectWorkEnvironments'
import { useProjects } from '@/composables/useProjects'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

const projects = useProjects()
const route = inject(routeLocationKey, null)
const router = inject(routerKey, null)
const routeProjectId = computed(() => {
  const id = typeof route?.query.projectId === 'string' ? route.query.projectId.trim() : ''
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
const selectedProjectId = ref<string | null>(routeProjectId.value)
const selectedProject = computed(() => projects.projects.kind === 'success' ? projects.projects.data.find((project) => project.id === selectedProjectId.value) ?? null : null)
const projectOptions = computed(() => projects.projects.kind === 'success' ? projects.projects.data : [])
const projectIdRef = computed(() => selectedProjectId.value)
const agent = useProjectAgentRun(projectIdRef)
const mode = ref<'configuration' | 'template'>(route?.query.mode === 'template' ? 'template' : 'configuration')

const policy = ref<AsyncState<ProjectLlmEgressPolicySchema>>({ kind: 'idle' })
const workEnvironments = useProjectWorkEnvironments(projectIdRef)
const selectedEnvironmentId = ref('')
const selectedEnvironment = computed(() => workEnvironments.environments.kind === 'success' ? workEnvironments.environments.data.find((environment) => environment.id === selectedEnvironmentId.value) ?? null : null)
const plan = ref<AsyncState<WorkConfigurationPlanViewSchema>>({ kind: 'idle' })
const planApprovalOutcome = ref<DiagnosticViewModel | null>(null)
const packageId = ref('')
const packageRevision = ref(1)
const packageInput = ref<HTMLInputElement | null>(null)
const impactAcknowledged = ref(false)
const approvalReason = ref('')
const restartConfirmed = ref(false)
const approvalExpiresAt = ref('')
const approving = ref(false)
const canStart = computed(() => Boolean(selectedProjectId.value && packageId.value.trim() && Number.isInteger(packageRevision.value) && packageRevision.value > 0 && selectedEnvironment.value && impactAcknowledged.value && policy.value.kind === 'success'))
const canApprovePlan = computed(() => {
  if (plan.value.kind !== 'success' || agent.run.kind !== 'success' || agent.run.data.state !== 'awaiting_approval') return false
  if (!approvalReason.value.trim() || !approvalExpiresAt.value) return false
  if (plan.value.data.plan.requiresRestart && !restartConfirmed.value) return false
  return new Date(approvalExpiresAt.value).getTime() > Date.now()
})

watch(
  () => projectOptions.value,
  (items) => {
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

let planRunId: string | null = null

function syncSoftwareRoute(
  runId: string | null | undefined = routeRunId.value,
  releaseId: string | null | undefined = routeReleaseId.value,
) {
  if (!router || !route) return
  void router.replace({
    query: {
      ...route.query,
      projectId: selectedProjectId.value ?? undefined,
      mode: mode.value === 'template' ? 'template' : undefined,
      runId: runId ?? undefined,
      releaseId: releaseId ?? undefined,
    },
  })
}

watch(selectedProjectId, (id, previousId) => {
  selectedEnvironmentId.value = ''
  planRunId = null
  plan.value = { kind: 'idle' }
  planApprovalOutcome.value = null
  approvalReason.value = ''
  restartConfirmed.value = false
  approvalExpiresAt.value = defaultApprovalExpiry()
  const sameProject = routeProjectId.value === id
  syncSoftwareRoute(
    previousId && previousId !== id ? null : sameProject ? routeRunId.value : null,
    previousId && previousId !== id ? null : sameProject ? routeReleaseId.value : null,
  )
  void reloadPolicy()
})

watch(mode, () => syncSoftwareRoute())

function persistTemplateRun(runId: string) {
  syncSoftwareRoute(runId, null)
}

function persistTemplateRelease(releaseId: string) {
  syncSoftwareRoute(routeRunId.value, releaseId)
}

watch(
  () => workEnvironments.environments,
  (state) => {
    if (state.kind === 'success') {
      if (!state.data.some((environment) => environment.id === selectedEnvironmentId.value)) selectedEnvironmentId.value = state.data[0]?.id ?? ''
    } else if (state.kind === 'empty') {
      selectedEnvironmentId.value = ''
    }
  },
  { immediate: true },
)

watch(
  () => agent.run,
  (state) => {
    if (state.kind !== 'success' || state.data.state !== 'awaiting_approval') {
      plan.value = { kind: 'idle' }
      return
    }
    if (planRunId !== state.data.id) {
      planRunId = state.data.id
      void reloadPlan(state.data.id)
    }
  },
)

async function reloadPolicy() {
  const id = selectedProjectId.value
  if (!id) {
    policy.value = { kind: 'idle' }
    return
  }
  policy.value = { kind: 'loading', message: '加载项目 LLM 策略…' }
  const result = await getActiveProjectLlmPolicy({ path: { projectId: id } })
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
  await agent.startWorkConfiguration({
    environmentId: environment.id,
    environmentRevision: environment.revision,
    packageId: packageId.value.trim(),
    packageRevision: packageRevision.value,
    policyId: policy.value.data.id,
    policyRevision: policy.value.data.revision,
    ...(selectedProject.value?.courseId ? { courseId: selectedProject.value.courseId } : {}),
  })
}

async function reloadPlan(runId?: string) {
  const id = selectedProjectId.value
  const resolvedRunId = runId ?? (agent.run.kind === 'success' && agent.run.data.state === 'awaiting_approval' ? agent.run.data.id : undefined)
  if (!id || !resolvedRunId) {
    plan.value = { kind: 'idle' }
    return
  }
  plan.value = { kind: 'loading', message: '加载 Work 配置计划…' }
  const result = await getProjectWorkConfigurationPlan({ path: { projectId: id, runId: resolvedRunId } })
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
  approving.value = true
  planApprovalOutcome.value = null
  const currentRun = agent.run.data
  const currentPlan = plan.value.data.plan
  const result = await approveProjectWorkConfigurationRun({
    path: { projectId: selectedProjectId.value, runId: currentRun.id },
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
  if (result.error) {
    const problem = extractProblemDetails(result.error)
    planApprovalOutcome.value = makeDiagnostic(problem?.diagnosticCode ?? 'PROJECT_WORK_PLAN_APPROVAL_FAILED', problem?.detail ?? '批准 Work 配置失败', problem?.retryable ?? true)
    approving.value = false
    return
  }
  if (!result.data) {
    planApprovalOutcome.value = makeDiagnostic('PROJECT_WORK_PLAN_APPROVAL_FAILED', '批准 Work 配置没有返回 AgentRun。', true)
    approving.value = false
    return
  }
  planApprovalOutcome.value = makeDiagnostic('PROJECT_WORK_PLAN_APPROVED', 'Work 配置已批准，正在执行。', false)
  approvalReason.value = ''
  restartConfirmed.value = false
  await agent.load(result.data.id)
  approving.value = false
}

function defaultApprovalExpiry() {
  const expiry = new Date(Date.now() + 15 * 60 * 1000)
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${expiry.getFullYear()}-${pad(expiry.getMonth() + 1)}-${pad(expiry.getDate())}T${pad(expiry.getHours())}:${pad(expiry.getMinutes())}`
}

function reloadRun() {
  if (agent.run.kind === 'success') return agent.load(agent.run.data.id)
  return undefined
}

function workConfigurationNeedsNewTask(data: AgentRunSchema) {
  return data.purpose.kind === 'work_configuration'
    && data.plan !== null
    && data.plan !== undefined
    && (data.state === 'failed' || data.state === 'partially_succeeded' || data.state === 'cancelled')
}

function prepareNewTask() {
  packageId.value = ''
  packageRevision.value = 1
  selectedEnvironmentId.value = ''
  impactAcknowledged.value = false
  plan.value = { kind: 'idle' }
  planApprovalOutcome.value = null
  packageInput.value?.focus()
}

function runStateLabel(state: string) {
  return ({ requested: 'Submitted', running: 'Running', awaiting_approval: 'Awaiting approval', partially_succeeded: 'Partially succeeded', succeeded: 'Succeeded', failed: 'Failed', cancelling: 'Cancelling', cancelled: 'Cancelled' } as Record<string, string>)[state] ?? state
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
.config-layout { display: grid; grid-template-columns: minmax(300px, .9fr) minmax(0, 1.3fr); gap: 20px; align-items: start; }
.config-card, .run-card { padding: 20px; }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.policy-summary, .run-overview, .plan-meta { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px 14px; margin: 18px 0; padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); font: var(--md-sys-body-small); }
.policy-summary span, .run-overview span, .plan-meta > div > span:first-child { color: var(--md-sys-color-on-surface-variant); }
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
