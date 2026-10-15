<template>
  <EnvironmentEntryView v-if="routeEnvironmentId" :key="routeEnvironmentId" />
  <div v-else class="work-page">
    <header class="work-header">
      <div>
        <h1 id="work-heading">Work 环境</h1>
        <p>手工选择已发布的模板创建 Work，再连接终端、桌面或 SSH。创建和连接不需要配置模型。</p>
      </div>
      <AgentAvailabilityNote :state="agent.state" :project-id="projectId" />
    </header>

    <DiagnosticBanner
      v-if="!projectId"
      code="PROJECT_CONTEXT_REQUIRED"
      message="请先在顶栏项目选择器中选择项目，再查看或创建 Work。"
      :retryable="false"
      severity="info"
    />

    <template v-else>
      <section class="work-section" aria-labelledby="existing-heading">
        <div class="section-head">
          <h2 id="existing-heading">项目中的 Work</h2>
          <button type="button" class="icon-button" aria-label="刷新 Work 列表" @click="workEnvironments.load">
            <SvgIcon name="refresh" size="sm" aria-hidden="true" />
          </button>
        </div>
        <AsyncStateView
          :state="workEnvironments.environments"
          empty-text="这个项目还没有 Work。在下方选择模板创建第一个 Work。"
          @retry="workEnvironments.load"
        >
          <template #success="{ data }">
            <ul class="work-list">
              <li v-for="environment in visibleEnvironments(data)" :key="environment.id" class="work-row md-card" :data-environment-id="environment.id">
                <div class="work-row__main">
                  <strong>{{ environment.displayLabel }}</strong>
                  <span class="work-row__meta">
                    {{ runtimeKindLabel(environment.runtimeKind) }} · 固定使用模板 v{{ environment.releaseVersion }} · 更新于 {{ formatTimestamp(environment.updatedAt) }}
                  </span>
                </div>
                <GcpStatusPill :state="environment.observedState" domain="environment" size="sm" />
                <RouterLink
                  class="outlined-button small"
                  :to="{ path: '/researcher/environments', query: { projectId, environmentId: environment.id } }"
                >
                  {{ environment.observedState === 'ready' ? '连接' : '打开' }}
                </RouterLink>
              </li>
            </ul>
            <p v-if="hiddenDeletedCount(data) > 0" class="section-hint">已隐藏 {{ hiddenDeletedCount(data) }} 个已删除的 Work。</p>
          </template>
        </AsyncStateView>
      </section>

      <section
        v-if="preparingRequests.length > 0 || endedRequests.length > 0 || requestsDiagnostic"
        class="work-section"
        aria-labelledby="preparing-heading"
        data-testid="preparing-work"
      >
        <h2 id="preparing-heading">Work 申请</h2>
        <DiagnosticBanner
          v-if="requestsDiagnostic"
          :code="requestsDiagnostic.code"
          :request-id="requestsDiagnostic.requestId"
          :message="`无法读取 Work 申请，可能有已提交但尚未创建的 Work：${requestsDiagnostic.message}`"
          :retryable="requestsDiagnostic.retryable"
          severity="warning"
          @retry="resources.load()"
        />
        <template v-if="preparingRequests.length > 0">
          <h3 class="subsection-title">准备中</h3>
          <ul class="work-list" data-testid="preparing-list">
            <li
              v-for="request in preparingRequests"
              :key="request.id"
              class="work-row md-card"
              :data-request-id="request.id"
              :data-request-state="request.state"
            >
              <div class="work-row__main">
                <strong>模板 v{{ request.target.releaseVersion }} · {{ resourceSummary(request.requestedResources) }}</strong>
                <span class="work-row__meta">申请于 {{ formatTimestamp(request.createdAt) }} · {{ requestStateText(request) }}</span>
                <span v-if="request.diagnosticCode" class="work-row__meta work-row__meta--error">
                  {{ requestDiagnosticText(request) }}
                </span>
              </div>
              <RouterLink
                class="outlined-button small"
                :to="requestProgressLink(request)"
              >
                {{ request.state === 'reviewing' ? '查看资源申请' : '查看准备进度' }}
              </RouterLink>
            </li>
          </ul>
          <p class="section-hint">审批通过后平台自动创建 Work，创建完成后会出现在“项目中的 Work”。页面在申请变化期间自动刷新。</p>
        </template>
        <template v-if="endedRequests.length > 0">
          <h3 class="subsection-title">未创建成功</h3>
          <ul class="work-list" data-testid="ended-list">
            <li
              v-for="request in endedRequests"
              :key="request.id"
              class="work-row work-row--ended md-card"
              :data-request-id="request.id"
              :data-request-state="request.state"
            >
              <div class="work-row__main">
                <strong>模板 v{{ request.target.releaseVersion }} · {{ resourceSummary(request.requestedResources) }}</strong>
                <span class="work-row__meta">申请于 {{ formatTimestamp(request.createdAt) }} · {{ requestStateText(request) }}</span>
                <span v-if="request.diagnosticCode" class="work-row__meta work-row__meta--error">
                  {{ requestDiagnosticText(request) }}
                </span>
              </div>
              <div class="work-row__actions">
                <RouterLink
                  class="text-button small"
                  :to="{ path: '/researcher/resources', query: { projectId } }"
                >
                  查看资源申请
                </RouterLink>
                <RouterLink
                  class="outlined-button small"
                  :to="{ path: '/researcher/resources', query: { projectId, releaseId: request.target.releaseId, releaseVersion: String(request.target.releaseVersion) } }"
                >
                  用同一模板重新申请
                </RouterLink>
              </div>
            </li>
          </ul>
          <p class="section-hint">这些申请已结束且没有创建 Work，不会再自动继续。需要 Work 时重新提交申请；重新申请会生成新的申请，旧申请保留在资源申请记录中。</p>
        </template>
      </section>

      <section class="work-section md-card create-card" aria-labelledby="create-heading">
        <h2 id="create-heading">新建 Work</h2>
        <ol class="create-steps" aria-label="新建 Work 的步骤">
          <li :aria-current="!selectedRelease ? 'step' : undefined">1. 选择模板</li>
          <li :aria-current="selectedRelease ? 'step' : undefined">2. 选择资源与时长，提交申请</li>
          <li>3. 审批通过后自动创建，进入环境连接</li>
        </ol>

        <AsyncStateView :state="usableReleases" @retry="releases.load">
          <template #success="{ data }">
            <RadioGroupRoot
              v-model="selectedReleaseKey"
              class="template-grid"
              aria-labelledby="create-heading"
              :aria-describedby="'template-pin-note'"
            >
              <RadioGroupItem
                v-for="release in data"
                :key="releaseKey(release)"
                :value="releaseKey(release)"
                class="template-card"
                :data-release-id="release.id"
              >
                <span class="template-card__radio" aria-hidden="true">
                  <RadioGroupIndicator class="template-card__dot" />
                </span>
                <span class="template-card__body">
                  <span class="template-card__title">
                    <SvgIcon :name="release.runtimeKind === 'virtual_machine' ? 'computer' : 'deployed_code'" size="sm" aria-hidden="true" />
                    {{ workTemplateTitle(release) }}
                  </span>
                  <span class="template-card__meta">发布于 {{ formatTimestamp(release.publishedAt) }}</span>
                  <span class="template-card__artifact">{{ workTemplateArtifactLabel(release) }}</span>
                </span>
              </RadioGroupItem>
            </RadioGroupRoot>
            <p id="template-pin-note" class="section-hint">
              创建后 Work 固定使用所选模板版本；模板发布新版本时，已有 Work 不会被自动替换。
            </p>
            <div class="create-actions">
              <RouterLink
                v-if="selectedRelease"
                class="filled-button"
                data-testid="continue-to-resources"
                :to="{ path: '/researcher/resources', query: { projectId, releaseId: selectedRelease.id, releaseVersion: String(selectedRelease.version) } }"
              >
                下一步：选择资源与时长
              </RouterLink>
              <button v-else type="button" class="filled-button" disabled>下一步：选择资源与时长</button>
              <span class="section-hint">申请提交后由管理员审批；这个页面会显示审批状态和创建进度。</span>
            </div>
          </template>
          <template #empty>
            <div class="templates-empty" data-testid="templates-empty">
              <SvgIcon name="inventory_2" size="lg" aria-hidden="true" />
              <p><strong>当前项目还没有可用的 Work 模板。</strong></p>
              <p>目前 Work 模板只能通过 Agent 生成、审核并发布；从平台镜像直接发布手工模板需要后端支持，当前版本还不能使用。模板发布后即可在这里手工选择并创建 Work。</p>
              <p v-if="agent.state.kind === 'available'" class="section-hint">
                当前项目已配置模型，可以生成 Work 模板。
                <RouterLink :to="{ path: '/researcher/software', query: { projectId } }">生成 Work 模板</RouterLink>
              </p>
              <p v-else class="section-hint">
                当前项目没有可用的模型，不能生成模板：请项目负责人在项目 AI 设置中配置模型，或联系课程教师发布已有模板。
              </p>
            </div>
          </template>
        </AsyncStateView>
      </section>

      <details class="advanced">
        <summary>高级：按环境 ID 打开</summary>
        <form class="advanced__form" @submit.prevent="openById">
          <label for="environment-id-input">环境 ID</label>
          <input id="environment-id-input" v-model.trim="environmentIdInput" class="text-input" autocomplete="off" spellcheck="false" />
          <button type="submit" class="outlined-button small" :disabled="!environmentIdInput">打开</button>
        </form>
      </details>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import { RadioGroupIndicator, RadioGroupItem, RadioGroupRoot } from 'reka-ui'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import GcpStatusPill from '@/components/common/GcpStatusPill.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import AgentAvailabilityNote from '@/components/work/AgentAvailabilityNote.vue'
import EnvironmentEntryView from '@/views/student/EnvironmentEntryView.vue'
import { useAgentAvailability } from '@/composables/useAgentAvailability'
import { useEnvironmentTemplateReleases } from '@/composables/useEnvironmentTemplateReleases'
import { useProjects } from '@/composables/useProjects'
import { useProjectWorkEnvironments } from '@/composables/useProjectWorkEnvironments'
import { resourceSummary, useProjectResources } from '@/composables/useProjectResources'
import type { EnvironmentSummary, EnvironmentTemplateReleaseViewSchema, ResourceRequestSchema } from '@/generated/contracts'
import type { AsyncState } from '@/types/async'
import { formatTimestamp } from '@/utils/format'
import { resourceAllocationFailureMessage } from '@/utils/stateLabels'
import { isUsableRelease, runtimeKindLabel, workTemplateArtifactLabel, workTemplateTitle } from '@/utils/workTemplate'

const route = useRoute()
const router = useRouter()
const projects = useProjects()

const routeEnvironmentId = computed(() => typeof route.query.environmentId === 'string' && route.query.environmentId.trim() ? route.query.environmentId.trim() : null)
const routeProjectId = computed(() => typeof route.query.projectId === 'string' && route.query.projectId.trim() ? route.query.projectId.trim() : null)
const projectId = computed(() => routeProjectId.value ?? projects.selectedProjectId ?? null)
const project = computed(() => projects.projects.kind === 'success' ? projects.projects.data.find((item) => item.id === projectId.value) ?? null : null)

// The launcher only loads when it is shown; an open environment owns its own data.
const launcherProjectId = computed(() => routeEnvironmentId.value ? null : projectId.value)
const workEnvironments = useProjectWorkEnvironments(launcherProjectId)
const agent = useAgentAvailability(launcherProjectId)
const releases = useEnvironmentTemplateReleases(
  computed(() => launcherProjectId.value ?? undefined),
  computed(() => project.value?.courseId ?? undefined),
)

/** Withdrawn templates cannot create new Work, so they are not offered. */
const usableReleases = computed<AsyncState<EnvironmentTemplateReleaseViewSchema[]>>(() => {
  const state = releases.releases
  if (state.kind !== 'success') return state
  const items = state.data.filter(isUsableRelease)
  return items.length ? { kind: 'success', data: items } : { kind: 'empty' }
})

const selectedReleaseKey = ref<string>('')
const selectedRelease = computed(() => {
  const state = usableReleases.value
  return state.kind === 'success' ? state.data.find((release) => releaseKey(release) === selectedReleaseKey.value) ?? null : null
})
watch(launcherProjectId, () => { selectedReleaseKey.value = '' })

type EnvironmentRequest = ResourceRequestSchema & { target: Extract<ResourceRequestSchema['target'], { kind: 'environment' }> }

const resources = useProjectResources(launcherProjectId)
const requestsDiagnostic = computed(() => {
  const state = resources.requests
  return state.kind === 'success' || state.kind === 'empty' || state.kind === 'idle' || state.kind === 'loading' ? null : state.diagnostic
})

/** Request states after which the server never creates the Work. */
const ENDED_REQUEST_STATES: Partial<Record<ResourceRequestSchema['state'], true>> = { expired: true, rejected: true, cancelled: true }

/**
 * Work requests whose environment is not in the Work list. A request that
 * already holds resources (`active`, `expiring`) is only reported as missing
 * its environment when the Work list was actually read.
 */
const requestsWithoutWork = computed<EnvironmentRequest[]>(() => {
  if (resources.requests.kind !== 'success') return []
  const list = workEnvironments.environments
  const listKnown = list.kind === 'success' || list.kind === 'empty'
  const created = new Set(list.kind === 'success' ? list.data.map((environment) => environment.id) : [])
  return resources.requests.data.filter((request): request is EnvironmentRequest => (
    request.target.kind === 'environment'
    && !created.has(request.target.environmentId)
    && (listKnown || request.state === 'reviewing' || request.state === 'allocating')
  ))
})
const preparingRequests = computed(() => requestsWithoutWork.value.filter((request) => !ENDED_REQUEST_STATES[request.state]))
const endedRequests = computed(() => requestsWithoutWork.value.filter((request) => ENDED_REQUEST_STATES[request.state]))

// Resource requests are re-read while they are still moving. Each new reading
// taken while Work is being prepared may mean an environment now exists, so the
// Work list is re-read in the background without blanking it.
let preparingBeforeRead = false
watch(() => resources.requests, () => {
  if (preparingBeforeRead || preparingRequests.value.length > 0) void workEnvironments.refresh()
  preparingBeforeRead = preparingRequests.value.length > 0
})

function requestStateText(request: EnvironmentRequest): string {
  switch (request.state) {
    case 'reviewing': return '等待管理员审批'
    case 'allocating': return '已批准，正在分配资源'
    case 'active': return '资源已分配，正在创建环境'
    case 'expiring': return '资源即将到期，环境尚未创建'
    case 'expired': return '申请已到期，没有创建 Work'
    case 'rejected': return '管理员已拒绝申请'
    case 'cancelled': return '申请已取消'
  }
}

function requestDiagnosticText(request: EnvironmentRequest): string {
  const reason = resourceAllocationFailureMessage(request.diagnosticCode)
  return reason ? `${reason}（${request.diagnosticCode}）` : `平台返回诊断码 ${request.diagnosticCode}。`
}

/** Before approval the request is the only thing to look at; afterwards the environment page shows provisioning. */
function requestProgressLink(request: EnvironmentRequest) {
  return request.state === 'reviewing'
    ? { path: '/researcher/resources', query: { projectId: projectId.value } }
    : { path: '/researcher/environments', query: { projectId: projectId.value, environmentId: request.target.environmentId } }
}

const environmentIdInput = ref('')

function releaseKey(release: Pick<EnvironmentTemplateReleaseViewSchema, 'id' | 'version'>) {
  return `${release.id}:${release.version}`
}

function visibleEnvironments(items: EnvironmentSummary[]) {
  return items.filter((item) => item.observedState !== 'deleted')
}

function hiddenDeletedCount(items: EnvironmentSummary[]) {
  return items.length - visibleEnvironments(items).length
}

function openById() {
  if (!environmentIdInput.value) return
  void router.push({ path: '/researcher/environments', query: { ...(projectId.value ? { projectId: projectId.value } : {}), environmentId: environmentIdInput.value } })
}
</script>

<style scoped>
.work-page { display: flex; flex-direction: column; gap: var(--space-6); max-width: 1120px; margin: 0 auto; }
.work-header { display: flex; flex-wrap: wrap; align-items: flex-start; justify-content: space-between; gap: var(--space-3) var(--space-6); }
.work-header h1 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-headline-small); }
.work-header p { margin: var(--space-1) 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.work-section { display: grid; gap: var(--space-3); }
.section-head { display: flex; align-items: center; justify-content: space-between; }
.work-section h2 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.section-hint { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.work-list { display: grid; gap: var(--space-2); margin: 0; padding: 0; list-style: none; }
.work-row { display: grid; grid-template-columns: minmax(0, 1fr) auto auto; align-items: center; gap: var(--space-3) var(--space-4); padding: var(--space-3) var(--space-4); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.work-row__main { display: grid; gap: 2px; min-width: 0; }
.work-row__main strong { overflow-wrap: anywhere; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.work-row__meta { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.work-row__meta--error { color: var(--md-sys-color-error); }
.work-row--ended { grid-template-columns: minmax(0, 1fr) auto; }
.work-row__actions { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2); }
.subsection-title { margin: var(--space-1) 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-large); }
.create-card { padding: var(--space-5); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-large); }
.create-steps { display: flex; flex-wrap: wrap; gap: var(--space-1) var(--space-5); margin: 0; padding: 0; list-style: none; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-large); }
.create-steps [aria-current='step'] { color: var(--md-sys-color-primary); }
.template-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(260px, 1fr)); gap: var(--space-3); }
.template-card { display: flex; gap: var(--space-3); align-items: flex-start; padding: var(--space-4) var(--space-4); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); background: var(--md-sys-color-surface); color: inherit; font: inherit; text-align: left; cursor: pointer; transition: border-color 120ms, background-color 120ms; }
.template-card:hover { background: var(--md-sys-color-surface-container-low); }
.template-card:focus-visible { outline: 2px solid var(--md-sys-color-primary); outline-offset: 2px; }
.template-card[data-state='checked'] { border-color: var(--md-sys-color-primary); background: var(--md-sys-color-primary-container); }
.template-card__radio { display: inline-grid; flex-shrink: 0; place-items: center; width: 20px; height: 20px; margin-top: 2px; border: 2px solid var(--md-sys-color-outline); border-radius: 50%; }
.template-card[data-state='checked'] .template-card__radio { border-color: var(--md-sys-color-primary); }
.template-card__dot { width: 10px; height: 10px; border-radius: 50%; background: var(--md-sys-color-primary); }
.template-card__body { display: grid; gap: var(--space-1); min-width: 0; }
.template-card__title { display: inline-flex; align-items: center; gap: var(--space-2); color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.template-card__meta, .template-card__artifact { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); overflow-wrap: anywhere; }
.create-actions { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2) var(--space-4); }
.templates-empty { display: grid; justify-items: start; gap: var(--space-2); padding: var(--space-2) 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.templates-empty p { margin: 0; }
.advanced { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.advanced__form { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2); margin-top: var(--space-2); }
.advanced__form .text-input { min-width: 280px; }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: none; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
.text-input { min-height: 40px; padding: 0 var(--space-3); border: 1px solid var(--md-sys-color-outline); border-radius: var(--md-sys-shape-extra-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
@media (max-width: 600px) {
  .work-row { grid-template-columns: minmax(0, 1fr) auto; }
  .work-row > :last-child { grid-column: 1 / -1; justify-self: start; }
  .advanced__form .text-input { min-width: 0; width: 100%; }
}
@media (prefers-reduced-motion: reduce) { .template-card { transition: none; } }
</style>
