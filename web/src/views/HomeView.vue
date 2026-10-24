<template>
  <div class="home-view">
    <DiagnosticBanner
      v-if="routeReason"
      class="route-reason"
      :code="routeReason.code"
      :message="routeReason.message"
      :retryable="false"
      severity="warning"
    />

    <template v-if="auth.isLoading.value" />

    <section v-else-if="!auth.isAuthenticated.value" class="empty-state" aria-labelledby="login-heading">
      <SvgIcon name="login" size="lg" aria-hidden="true" />
      <h1 id="login-heading">欢迎进入 LabWeaver</h1>
      <p>请使用组织账号登录后查看当前项目的进行中事项。</p>
      <button v-if="oidcEnabled" type="button" class="filled-button" @click="auth.login()">
        <SvgIcon name="login" size="sm" aria-hidden="true" />
        <span>登录</span>
      </button>
      <p v-else class="hint">当前部署未配置 OIDC 登录服务。</p>
    </section>

    <template v-else>
      <header class="home-header">
        <div class="home-header__text">
          <p class="eyebrow">进行中</p>
          <h1>{{ project?.name ?? '当前项目' }}</h1>
          <p class="home-header__meta" role="status">
            <template v-if="inProgress.loading">正在刷新…</template>
            <template v-else-if="inProgress.loadedAt">更新于 {{ formatClock(inProgress.loadedAt) }} · 页面在后台时不自动刷新</template>
          </p>
        </div>
        <button
          v-if="project"
          type="button"
          class="outlined-button"
          :disabled="inProgress.loading"
          aria-label="刷新进行中事项"
          @click="inProgress.load()"
        >
          <SvgIcon name="refresh" size="sm" aria-hidden="true" />
          <span>刷新</span>
        </button>
      </header>

      <DiagnosticBanner
        v-if="projects.projects.kind === 'error'"
        :diagnostic="projects.projects.diagnostic"
        @retry="projects.load()"
      />

      <section v-else-if="!project && projects.projects.kind !== 'loading'" class="empty-state" aria-labelledby="no-project-heading">
        <SvgIcon name="folder_off" size="lg" aria-hidden="true" />
        <h2 id="no-project-heading">还没有可访问的项目</h2>
        <p>创建项目后，这里会显示项目中运行的环境、未完成的任务和待处理事项。也可以联系项目 Owner 邀请你加入。</p>
        <RouterLink class="filled-button" :to="{ path: '/researcher/workspaces' }">前往项目与工作空间</RouterLink>
      </section>

      <div v-else-if="project" class="home-sections">
        <section class="home-section md-card" aria-labelledby="home-env-heading" data-home-section="environments">
          <div class="section-heading">
            <h2 id="home-env-heading">运行中的环境</h2>
            <span v-if="inProgress.environments.kind === 'success'" class="count">{{ inProgress.environments.data.length }}</span>
          </div>
          <AsyncStateView
            v-if="inProgress.environments.kind !== 'hidden'"
            :state="inProgress.environments"
            empty-text="当前项目没有运行中的环境。"
            @retry="inProgress.load()"
          >
            <template #success="{ data }">
              <ul class="item-list">
                <li v-for="env in data" :key="env.id" class="item-row" :class="{ 'item-row--attention': environmentNeedsAttention(env) }">
                  <div class="item-main">
                    <div class="item-title">
                      <strong>{{ env.displayLabel }}</strong>
                      <GcpStatusPill :state="env.observedState" domain="environment" size="sm" />
                    </div>
                    <p class="item-sub">
                      {{ env.class === 'work' ? 'Work 环境' : '实验环境' }} · {{ env.runtimeKind === 'virtual_machine' ? '虚拟机' : '容器' }}
                      <template v-if="environmentOperationText(env)"> · {{ environmentOperationText(env) }}</template>
                    </p>
                    <p v-if="environmentNeedsAttention(env)" class="item-attention">
                      环境处理失败。打开控制台查看原因，可在“异步操作与诊断”中重试或删除。
                    </p>
                  </div>
                  <RouterLink class="outlined-button" :to="environmentLink(env)">
                    {{ environmentNeedsAttention(env) ? '查看原因' : '打开控制台' }}
                  </RouterLink>
                </li>
              </ul>
            </template>
          </AsyncStateView>
          <p v-if="inProgress.environmentsTruncated" class="section-note">
            只显示前 50 个运行中的环境。
            <RouterLink :to="{ path: '/researcher/environments', query: { projectId: project.id } }">查看全部 Work 环境</RouterLink>
          </p>
        </section>

        <section
          v-if="inProgress.agentRuns.kind !== 'hidden'"
          class="home-section md-card"
          aria-labelledby="home-runs-heading"
          data-home-section="agent-runs"
        >
          <div class="section-heading">
            <h2 id="home-runs-heading">未完成的生成与配置</h2>
            <span v-if="inProgress.agentRuns.kind === 'success'" class="count">{{ inProgress.agentRuns.data.length }}</span>
          </div>
          <AsyncStateView :state="inProgress.agentRuns" empty-text="当前项目没有未完成的生成或配置任务。" @retry="inProgress.load()">
            <template #success="{ data }">
              <ul class="item-list">
                <li
                  v-for="run in data"
                  :key="run.id"
                  class="item-row"
                  :class="{ 'item-row--attention': runAttention(run).length > 0 }"
                  :data-run-id="run.id"
                >
                  <div class="item-main">
                    <div class="item-title">
                      <strong>{{ agentRunPurposeLabel(run) }}</strong>
                      <GcpStatusPill :state="run.state" domain="agent" size="sm" />
                    </div>
                    <p class="item-sub">创建于 {{ formatTimestamp(run.createdAt) }} · 更新于 {{ formatTimestamp(run.updatedAt) }}</p>
                    <ul
                      v-if="runTracks(run.id).length"
                      class="track-list"
                      :aria-label="`${agentRunPurposeLabel(run)}的执行进度`"
                    >
                      <li v-for="track in runTracks(run.id)" :key="track.track" class="track-chip" :data-track="track.track">
                        <span class="track-chip__name">{{ track.trackLabel }}</span>
                        <GcpStatusPill :state="track.phase" domain="custom" :tone="track.tone" :label="track.phaseLabel" :title="track.phaseDetail" size="sm" />
                        <span v-if="track.attempt" class="track-chip__attempt">第 {{ track.attempt }} 次尝试</span>
                      </li>
                    </ul>
                    <p v-else-if="inProgress.agentProgress[run.id]?.kind === 'loading'" class="track-loading" aria-hidden="true">
                      <span class="track-skeleton" /><span class="track-skeleton" />
                    </p>
                    <p v-for="hint in runAttention(run)" :key="hint" class="item-attention" data-run-attention>{{ hint }}</p>
                    <RouterLink
                      v-if="runWaitsForResourceApproval(run.id)"
                      class="text-button small track-resource-link"
                      :to="{ path: '/researcher/resources', query: { projectId: run.projectId } }"
                    >
                      查看资源申请
                    </RouterLink>
                    <DiagnosticBanner
                      v-if="runProgressDiagnostic(run.id)"
                      class="track-diagnostic"
                      severity="warning"
                      :diagnostic="runProgressDiagnostic(run.id)"
                      @retry="inProgress.retryAgentProgress(run.id)"
                    />
                  </div>
                  <RouterLink v-if="agentRunLink(run)" class="outlined-button" :to="agentRunLink(run)!">
                    {{ run.state === 'awaiting_approval' ? '去审核' : '查看进度' }}
                  </RouterLink>
                </li>
              </ul>
            </template>
          </AsyncStateView>
          <p v-if="inProgress.agentRunsTruncated" class="section-note">
            只检查了最近 50 个任务，更早的未完成任务可能没有列出。
          </p>
        </section>

        <section
          v-if="inProgress.resourceRequests.kind !== 'hidden'"
          class="home-section md-card"
          aria-labelledby="home-requests-heading"
          data-home-section="resource-requests"
        >
          <div class="section-heading">
            <h2 id="home-requests-heading">待处理事项</h2>
            <span v-if="inProgress.resourceRequests.kind === 'success'" class="count">{{ inProgress.resourceRequests.data.length }}</span>
          </div>
          <AsyncStateView :state="inProgress.resourceRequests" empty-text="没有找到待审批或分配中的资源申请。" @retry="inProgress.load()">
            <template #success="{ data }">
              <ul class="item-list">
                <li v-for="request in data" :key="request.id" class="item-row" :class="{ 'item-row--attention': resourceRequestAttention(request) }">
                  <div class="item-main">
                    <div class="item-title">
                      <strong>{{ resourceSummary(request) }}</strong>
                      <GcpStatusPill :state="request.state" domain="resource" size="sm" />
                    </div>
                    <p class="item-sub">{{ request.target.kind === 'environment' ? '环境容量' : '任务容量' }} · 更新于 {{ formatTimestamp(request.updatedAt) }}</p>
                    <p v-if="resourceRequestAttention(request)" class="item-attention">{{ resourceRequestAttention(request) }}</p>
                  </div>
                  <RouterLink class="outlined-button" :to="{ path: '/researcher/resources', query: { projectId: project.id } }">
                    查看申请
                  </RouterLink>
                </li>
              </ul>
            </template>
          </AsyncStateView>
          <p
            v-if="inProgress.resourceRequests.kind === 'success' || inProgress.resourceRequests.kind === 'empty'"
            class="section-note"
            data-home-note="resource-requests-incomplete"
          >
            这里的列表可能不完整：平台暂时只返回你能看到的申请，且不分页。
            <RouterLink :to="{ path: '/researcher/resources', query: { projectId: project.id } }">在资源申请页查看全部</RouterLink>
          </p>
        </section>
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { RouterLink, useRoute, type RouteLocationRaw } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import GcpStatusPill from '@/components/common/GcpStatusPill.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useAuth } from '@/composables/useAuth'
import { useProjectInProgress } from '@/composables/useProjectInProgress'
import { useProjects } from '@/composables/useProjects'
import { OIDC_ENABLED } from '@/config'
import type { AgentRunHistoryItem, EnvironmentSummary, ResourceRequestSchema } from '@/generated/contracts'
import type { DiagnosticViewModel } from '@/types/async'
import { readAgentTaskProgress, type AgentTrackReading } from '@/utils/agentTaskProgress'
import { formatBytes, formatTimestamp } from '@/utils/format'
import { hasAnyRole, rolesFromProfile } from '@/utils/navigation'
import { environmentOperationKindLabel, resourceAllocationFailureMessage } from '@/utils/stateLabels'

const auth = useAuth()
const projects = useProjects()
const route = useRoute()
const oidcEnabled = OIDC_ENABLED

const roles = computed(() => auth.isAuthenticated.value ? rolesFromProfile(auth.user.value?.profile) : [])
// The AppShell keeps the selection equal to the linked project before this view renders.
const project = computed(() => auth.isAuthenticated.value ? projects.selectedProject : null)
const projectId = computed(() => project.value?.id ?? null)
const inProgress = useProjectInProgress(projectId)

function environmentNeedsAttention(env: EnvironmentSummary): boolean {
  return env.observedState === 'failed' || env.currentOperation?.state === 'failed'
}

function environmentOperationText(env: EnvironmentSummary): string {
  const operation = env.currentOperation
  if (!operation || !['accepted', 'running', 'cancelling'].includes(operation.state)) return ''
  return operation.state === 'cancelling' ? `正在取消${environmentOperationKindLabel(operation.kind)}` : `正在${environmentOperationKindLabel(operation.kind)}`
}

function environmentLink(env: EnvironmentSummary): RouteLocationRaw {
  const studentConsole = env.class === 'experiment' && roles.value.includes('student')
  return {
    path: studentConsole ? '/student/environments' : '/researcher/environments',
    query: { projectId: env.projectId, environmentId: env.id },
  }
}

function agentRunPurposeLabel(run: AgentRunHistoryItem): string {
  if (run.purpose.kind === 'work_configuration') return 'Work 配置'
  return run.purpose.environmentClass === 'work' ? 'Work 模板生成' : '实验生成'
}

function runTracks(runId: string): AgentTrackReading[] {
  const state = inProgress.agentProgress[runId]
  return state?.kind === 'ready' ? readAgentTaskProgress(state.progress) : []
}

/** What a person has to do next; the per-track reading wins over the aggregate run state. */
function runAttention(run: AgentRunHistoryItem): string[] {
  const fromTracks = runTracks(run.id).flatMap((track) => track.attention ? [track.attention] : [])
  if (fromTracks.length) return fromTracks
  return run.state === 'awaiting_approval' ? ['等待人工审核。审核通过后才会继续。'] : []
}

function runWaitsForResourceApproval(runId: string): boolean {
  return runTracks(runId).some((track) => track.phase === 'waiting_resource_approval')
}

function runProgressDiagnostic(runId: string): DiagnosticViewModel | null {
  const state = inProgress.agentProgress[runId]
  return state?.kind === 'error' ? state.diagnostic : null
}

/** The page that owns the run; awaiting approval opens the review entry directly. */
function agentRunLink(run: AgentRunHistoryItem): RouteLocationRaw | null {
  const query = { projectId: run.projectId, runId: run.id }
  if (run.purpose.kind === 'work_configuration') return { path: '/researcher/software', query }
  if (run.purpose.environmentClass === 'work') return { path: '/researcher/software', query: { ...query, mode: 'template' } }
  if (!hasAnyRole(roles.value, ['teacher'])) return null
  return { path: run.state === 'awaiting_approval' ? '/teacher/approvals' : '/teacher/materials', query }
}

function resourceSummary(request: ResourceRequestSchema): string {
  const resources = request.requestedResources
  const cores = resources.cpuMillicores / 1000
  const parts = [`${Number.isInteger(cores) ? cores : cores.toFixed(1)} 核 CPU`, `${formatBytes(resources.memoryBytes)} 内存`]
  if (resources.gpu) parts.push('GPU')
  return parts.join(' · ')
}

function resourceRequestAttention(request: ResourceRequestSchema): string {
  const failure = resourceAllocationFailureMessage(request.diagnosticCode)
  if (failure) return failure
  if (request.state === 'reviewing') return '等待管理员审批。'
  if (request.state === 'expiring') return '使用授权即将到期，请决定是否续期。'
  return ''
}

function formatClock(date: Date): string {
  return date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
}

const routeReason = computed(() => {
  const reason = route.query.reason
  if (!reason || Array.isArray(reason)) return null
  switch (reason) {
    case 'unauthorized':
      return { code: 'unauthorized', message: '当前账号没有该页面的访问权限。' }
    case 'auth-not-configured':
      return { code: 'auth-not-configured', message: '当前部署未配置身份验证服务，无法访问受保护页面。' }
    case 'callback-failed':
      return { code: 'callback-failed', message: '登录回调处理失败，请重试。' }
    default:
      return { code: 'access-denied', message: '无法访问该页面。' }
  }
})
</script>

<style scoped>
.home-view {
  display: flex;
  flex-direction: column;
  gap: var(--space-5);
  max-width: var(--content-max-width);
  margin: 0 auto;
}

.home-header {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: var(--space-4);
}

.home-header__text {
  display: grid;
  min-width: 0;
  gap: var(--space-1);
}

.eyebrow {
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
}

.home-header h1 {
  overflow: hidden;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-headline-small);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.home-header__meta {
  min-height: 1.4em;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.home-sections {
  display: grid;
  gap: var(--space-4);
}

.home-section {
  display: grid;
  gap: var(--space-2);
  padding: var(--space-4) var(--space-5);
}

.section-heading {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  padding-bottom: var(--space-3);
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.section-heading h2 {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-medium);
}

.count {
  min-width: 24px;
  padding: 0 var(--space-2);
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-surface-container-high);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-medium);
  font-variant-numeric: tabular-nums;
  line-height: 22px;
  text-align: center;
}

.item-list {
  display: grid;
  margin: 0;
  padding: 0;
  list-style: none;
}

.item-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-4);
  padding: var(--space-3) 0;
  border-top: 1px solid var(--md-sys-color-outline-variant);
}

.item-row:first-child {
  border-top: 0;
}

.item-row--attention {
  box-shadow: inset 3px 0 0 var(--md-sys-color-warning);
  padding-left: var(--space-3);
}

.item-main {
  display: grid;
  min-width: 0;
  gap: var(--space-1);
}

.item-title {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-2);
}

.item-title strong {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-large);
  font-weight: 500;
}

.item-sub {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.item-attention {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
}

.track-list {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2) var(--space-3);
  margin: var(--space-1) 0 0;
  padding: 0;
  list-style: none;
}

.track-chip {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  min-height: 28px;
  padding: 0 var(--space-2) 0 var(--space-3);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container-low);
}

.track-chip__name {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-label-medium);
}

.track-chip__attempt {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-small);
  font-variant-numeric: tabular-nums;
}

.track-loading {
  display: flex;
  gap: var(--space-2);
  margin: var(--space-1) 0 0;
}

.track-skeleton {
  width: 120px;
  height: 28px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container-high);
  animation: track-pulse 1.4s ease-in-out infinite;
}

@keyframes track-pulse {
  50% { opacity: 0.5; }
}

@media (prefers-reduced-motion: reduce) {
  .track-skeleton { animation: none; }
}

.track-resource-link {
  justify-self: start;
}

.track-diagnostic {
  margin-top: var(--space-1);
}

.section-note {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.section-note a {
  color: var(--md-sys-color-primary);
}

.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-12) var(--space-6);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-large);
  background: var(--md-sys-color-surface-container-high);
  color: var(--md-sys-color-on-surface-variant);
  text-align: center;
}

.empty-state h1,
.empty-state h2 {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-large);
}

/* Colors, heights and disabled tone come from styles/components.css. */
.filled-button,
.outlined-button {
  flex-shrink: 0;
}

.hint {
  font: var(--md-sys-body-small);
}

@media (max-width: 599px) {
  .home-header,
  .item-row {
    align-items: stretch;
    flex-direction: column;
  }

  .home-header h1 {
    white-space: normal;
  }

  /* Narrow screens: sections become flat list cards to shorten the page. */
  .home-section {
    padding: var(--space-3) var(--space-4);
    box-shadow: none;
  }
}
</style>
