<template>
  <div class="policy-page">
    <header class="page-header">
      <div>
        <p class="eyebrow">Project</p>
        <h2>项目 AI 设置</h2>
        <p class="page-subtitle">选择项目可用模型、材料出站范围和 Agent 调用预算。身份凭据由服务端配置并不会在浏览器中收集。</p>
      </div>
      <button type="button" class="icon-button" aria-label="刷新项目 AI 设置" :disabled="loading" @click="loadPolicyContext">
        <SvgIcon name="refresh" size="sm" aria-hidden="true" />
      </button>
    </header>

    <section class="project-strip md-card" aria-label="项目上下文">
      <label>
        <span>项目</span>
        <select
          v-model="selectedProjectId"
          class="text-input"
          data-testid="policy-project-select"
          :disabled="projects.projects.kind !== 'success' || loading"
        >
          <option value="">选择项目</option>
          <option v-for="project in projectOptions" :key="project.id" :value="project.id">
            {{ project.name }}
          </option>
        </select>
      </label>
      <span v-if="selectedProject" class="project-scope">
        {{ selectedProject.courseId ? `课程 ${selectedProject.courseId}` : '独立科研项目' }}
      </span>
    </section>

    <DiagnosticBanner
      v-if="projectContextUnavailable"
      code="PROJECT_CONTEXT_UNAVAILABLE"
      message="链接中的项目不存在或你无权访问，已停止加载项目 AI 设置。请从项目与工作空间重新选择。"
      :retryable="false"
      severity="warning"
    />
    <RouterLink v-if="projectContextUnavailable" class="outlined-button project-context-action" to="/researcher/workspaces">
      打开项目与工作空间
    </RouterLink>

    <template v-if="selectedProjectId && !projectContextUnavailable">
      <AsyncStateView :state="optionsState" data-testid="policy-options-state" @retry="loadPolicyContext">
        <template #success="{ data: options }">
          <section class="policy-layout">
            <section class="policy-summary md-card" aria-labelledby="policy-summary-heading">
              <div class="section-heading">
                <div>
                  <h3 id="policy-summary-heading">当前项目 AI 设置</h3>
                  <p>材料仅会按项目批准的材料范围提交给生成服务。</p>
                </div>
                <span v-if="activePolicy" class="state-chip state-chip--ready">已启用</span>
                <span v-else class="state-chip">尚未配置</span>
              </div>

              <AsyncStateView :state="policyState" empty-text="当前项目还没有已激活的 AI 设置。完成下方表单后，材料上传和 Work 配置才可使用。" @retry="loadPolicyContext">
                <template #success="{ data }">
                  <dl class="summary-grid">
                    <div><dt>模型</dt><dd>{{ modelLabel(data.binding.model, options) }}</dd></div>
                    <div><dt>材料范围</dt><dd>仅限项目材料清单明确允许的路径</dd></div>
                    <div><dt>预算</dt><dd>{{ data.budget.maxRequests }} 次请求 · {{ data.budget.maxInputTokens }} 输入 tokens</dd></div>
                    <div><dt>最近更新</dt><dd>{{ formatTimestamp(data.activatedAt) }}</dd></div>
                  </dl>
                </template>
                <template #empty>
                  <div class="empty-policy" data-testid="policy-empty-state">
                    <SvgIcon name="info" size="lg" aria-hidden="true" />
                    <p>当前项目还没有已激活的 AI 设置。完成下方表单后，材料上传和 Work 配置才可使用。</p>
                  </div>
                </template>
              </AsyncStateView>
            </section>

            <form class="policy-form md-card" data-testid="policy-form" @submit.prevent="savePolicy">
              <div class="section-heading">
                <div>
                  <h3>{{ activePolicy ? '编辑项目 AI 设置' : '创建项目 AI 设置' }}</h3>
                  <p>模型由平台部署配置提供。这里不会要求输入 API 密钥、服务地址或内部运行时标识。</p>
                </div>
              </div>

              <label>
                <span>可用模型</span>
                <select v-model="model" class="text-input" data-testid="policy-model-select" :disabled="saving" required>
                  <option value="" disabled>选择模型</option>
                  <option v-for="option in options.models" :key="option.model" :value="option.model">
                    {{ option.label }}
                  </option>
                </select>
              </label>

              <fieldset class="scope-fieldset">
                <legend>材料出站范围</legend>
                <label class="authorization-field">
                  <input v-model="materialsConsent" data-testid="policy-material-consent" type="checkbox" :disabled="saving" />
                  <span>我确认仅将项目材料清单明确允许的文件提交给生成服务，未获授权的学生提交、密钥和个人信息会被平台拦截。</span>
                </label>
                <p class="field-help">项目材料清单负责限定可提交路径；平台始终拒绝密钥、令牌、私钥、个人信息和未获授权的学生提交。</p>
                <RouterLink
                  v-if="selectedProjectId"
                  class="inline-link"
                  :to="{ path: '/teacher/materials', query: { projectId: selectedProjectId } }"
                >
                  查看项目材料清单
                </RouterLink>
              </fieldset>

              <fieldset class="budget-fieldset">
                <legend>单次 Agent 预算</legend>
                <div class="budget-grid">
                  <label>
                    <span>最大输入 tokens</span>
                    <input v-model.number="budget.maxInputTokens" name="maxInputTokens" data-testid="policy-budget-max-input-tokens" class="text-input" type="number" min="1" step="1" :disabled="saving" required />
                  </label>
                  <label>
                    <span>最大输出 tokens</span>
                    <input v-model.number="budget.maxOutputTokens" name="maxOutputTokens" data-testid="policy-budget-max-output-tokens" class="text-input" type="number" min="1" step="1" :disabled="saving" required />
                  </label>
                  <label>
                    <span>最大请求次数</span>
                    <input v-model.number="budget.maxRequests" name="maxRequests" data-testid="policy-budget-max-requests" class="text-input" type="number" min="1" step="1" :disabled="saving" required />
                  </label>
                  <label>
                    <span>最大成本（美元）</span>
                    <input v-model="budgetCostDollars" name="maxCostDollars" class="text-input" type="number" min="0.000001" step="0.000001" inputmode="decimal" :disabled="saving" required />
                  </label>
                  <label>
                    <span>超时（秒）</span>
                    <input v-model="budgetTimeoutSeconds" name="timeoutSeconds" class="text-input" type="number" min="0.001" step="0.001" inputmode="decimal" :disabled="saving" required />
                  </label>
                </div>
                <details class="advanced-settings" open>
                  <summary>高级设置：失败重试与结构修复</summary>
                  <p class="field-help">只影响临时网络失败和结构化响应修复；平台不会绕过材料范围或硬拒绝规则。</p>
                  <div class="budget-grid">
                    <label>
                      <span>临时重试次数（最多 2）</span>
                      <input v-model.number="budget.maxTransientRetries" name="maxTransientRetries" class="text-input" type="number" min="0" max="2" step="1" :disabled="saving" required />
                    </label>
                    <label>
                      <span>结构修复次数（最多 2）</span>
                      <input v-model.number="budget.maxSchemaRepairs" name="maxSchemaRepairs" class="text-input" type="number" min="0" max="2" step="1" :disabled="saving" required />
                    </label>
                  </div>
                </details>
              </fieldset>

              <DiagnosticBanner
                v-if="saveDiagnostic"
                :code="saveDiagnostic.code"
                :message="saveDiagnostic.message"
                :retryable="saveDiagnostic.retryable"
                :severity="saveDiagnostic.code === 'LW_AUTH_SCOPE_DENIED' || saveDiagnostic.code === 'LW_ACCESS_DENIED' ? 'warning' : 'error'"
                @retry="loadPolicyContext"
              />
              <p v-if="validationMessage" class="validation-message" role="alert">{{ validationMessage }}</p>
              <div class="form-actions">
                <button type="submit" class="filled-button" data-testid="policy-save-button" :disabled="!canSave">
                  {{ saving ? '保存中…' : activePolicy ? '保存项目 AI 设置' : '创建项目 AI 设置' }}
                </button>
                <span class="form-note">保存后，新的 Agent 任务会使用更新后的项目 AI 设置；已运行任务继续使用启动时的设置。</span>
              </div>
            </form>
          </section>
        </template>
      </AsyncStateView>
    </template>

    <AsyncStateView v-else-if="projects.projects.kind !== 'success'" :state="projects.projects" empty-text="还没有可配置的项目。请先创建或加入一个项目。" @retry="projects.load" />
    <div v-else class="empty-page md-card">
      <SvgIcon name="folder_open" size="xl" aria-hidden="true" />
      <h3>选择一个项目</h3>
      <p>项目 AI 设置必须绑定到具体项目，不能脱离项目单独配置。</p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useProjects } from '@/composables/useProjects'
import {
  createProjectLlmPolicy,
  getActiveProjectLlmPolicy,
  getProjectLlmPolicyOptions,
} from '@/generated/contracts'
import type {
  ProjectLlmEgressPolicySchema,
  ProjectLlmPolicyOptionsSchema,
  ProjectSchema,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { formatTimestamp, idempotencyKey, ifMatch, newUuidV7 } from '@/utils/format'

const projects = useProjects()
const route = useRoute()
const router = useRouter()
const projectOptions = computed(() => projects.projects.kind === 'success' ? projects.projects.data : [])
const routeProjectId = computed(() => {
  const value = route.query.projectId
  const projectId = Array.isArray(value) ? value[0] : value
  return typeof projectId === 'string' && projectId.trim() ? projectId.trim() : undefined
})
const projectContextUnavailable = computed(() => Boolean(
  routeProjectId.value
  && projects.projects.kind === 'success'
  && !projectOptions.value.some((project) => project.id === routeProjectId.value),
))
const selectedProjectId = computed<string | null>({
  get: () => {
    if (projectContextUnavailable.value) return null
    if (routeProjectId.value) return projects.projects.kind === 'success' ? routeProjectId.value : null
    return projects.selectedProjectId
  },
  set: (projectId) => {
    if (!projectId) return
    if (projectId !== projects.selectedProjectId) projects.select(projectId)
    if (route.query.projectId !== projectId) void router.replace({ query: { ...route.query, projectId } })
  },
})
const selectedProject = computed<ProjectSchema | null>(() => projectOptions.value.find((project) => project.id === selectedProjectId.value) ?? null)

const optionsState = ref<AsyncState<ProjectLlmPolicyOptionsSchema>>({ kind: 'idle' })
const policyState = ref<AsyncState<ProjectLlmEgressPolicySchema>>({ kind: 'idle' })
const saving = ref(false)
const saveDiagnostic = ref<DiagnosticViewModel | null>(null)
let loadGeneration = 0

const model = ref('')
const materialsConsent = ref(false)
const budget = reactive({
  maxInputTokens: 120_000,
  maxOutputTokens: 16_000,
  maxRequests: 8,
  maxCostMicrousd: 2_000_000,
  timeoutMilliseconds: 120_000,
  maxTransientRetries: 2,
  maxSchemaRepairs: 2,
})
const budgetCostDollars = ref('2')
const budgetTimeoutSeconds = ref('120')

function parseFixedDecimal(value: unknown, scale: number): number | null {
  const text = String(value ?? '').trim()
  const pattern = new RegExp(`^(?:0|[1-9][0-9]*)(?:\\.[0-9]{1,${scale}})?$`)
  if (!pattern.test(text)) return null
  const [wholePart, fractionPart = ''] = text.split('.')
  const whole = Number(wholePart)
  const fraction = Number((fractionPart + '0'.repeat(scale)).slice(0, scale))
  const multiplier = 10 ** scale
  const result = whole * multiplier + fraction
  return Number.isSafeInteger(result) && result > 0 ? result : null
}

function formatFixedDecimal(value: number, scale: number): string {
  const multiplier = 10 ** scale
  const whole = Math.floor(value / multiplier)
  const fraction = value % multiplier
  if (fraction === 0) return String(whole)
  return `${whole}.${String(fraction).padStart(scale, '0').replace(/0+$/, '')}`
}

const costMicrousd = computed(() => parseFixedDecimal(budgetCostDollars.value, 6))
const timeoutMilliseconds = computed(() => parseFixedDecimal(budgetTimeoutSeconds.value, 3))

const activePolicy = computed(() => policyState.value.kind === 'success' ? policyState.value.data : null)
const loading = computed(() => optionsState.value.kind === 'loading' || policyState.value.kind === 'loading')
const validationMessage = computed(() => {
  if (!selectedProjectId.value || optionsState.value.kind !== 'success') return ''
  if (!materialsConsent.value) return '请先确认材料出站范围，平台才会允许保存项目 AI 设置。'
  if (!model.value || !optionsState.value.data.models.some((option) => option.model === model.value)) return '请选择平台提供的有效模型。'
  const positiveIntegers = [budget.maxInputTokens, budget.maxOutputTokens, budget.maxRequests]
  if (positiveIntegers.some((value) => !Number.isInteger(value) || value <= 0)) return '预算中的 token 和请求次数必须是正整数。'
  if (costMicrousd.value === null || timeoutMilliseconds.value === null) return '成本请使用最多 6 位小数的美元金额，超时请使用最多 3 位小数的秒数。'
  if (![budget.maxTransientRetries, budget.maxSchemaRepairs].every((value) => Number.isInteger(value) && value >= 0 && value <= 2)) return '重试和结构修复次数必须是 0 到 2。'
  return ''
})
const canSave = computed(() => Boolean(
  selectedProjectId.value
  && optionsState.value.kind === 'success'
  && !loading.value
  && !saving.value
  && !validationMessage.value,
))

function diagnostic(error: unknown, fallbackCode: string, fallbackMessage: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  const code = problem?.diagnosticCode ?? fallbackCode
  if (code === 'LW_AUTH_SCOPE_DENIED' || code === 'LW_ACCESS_DENIED') {
    return makeDiagnostic(code, '当前账号没有修改这个项目 AI 设置的权限。请联系项目负责人或管理员，或切换到有权限的项目。', false)
  }
  if (code === 'LW_REVISION_CONFLICT' || code === 'LW_IF_MATCH_REQUIRED') {
    return makeDiagnostic(code, '项目 AI 设置已被其他人更新，请刷新后重新检查，再保存你的修改。', false)
  }
  return makeDiagnostic(code, problem?.detail ?? fallbackMessage, problem?.retryable ?? true)
}

function syncForm(options: ProjectLlmPolicyOptionsSchema, policy: ProjectLlmEgressPolicySchema | null) {
  model.value = policy?.binding.model ?? options.defaultModel
  materialsConsent.value = policy?.studentContentMode === 'manifest_allowlist_only'
  if (!policy) {
    budget.maxInputTokens = 120_000
    budget.maxOutputTokens = 16_000
    budget.maxRequests = 8
    budget.maxCostMicrousd = 2_000_000
    budget.timeoutMilliseconds = 120_000
    budget.maxTransientRetries = 2
    budget.maxSchemaRepairs = 2
    budgetCostDollars.value = '2'
    budgetTimeoutSeconds.value = '120'
    return
  }
  Object.assign(budget, policy.budget)
  budgetCostDollars.value = formatFixedDecimal(policy.budget.maxCostMicrousd, 6)
  budgetTimeoutSeconds.value = formatFixedDecimal(policy.budget.timeoutMilliseconds, 3)
}

async function loadPolicyContext() {
  const id = selectedProjectId.value
  const generation = ++loadGeneration
  saveDiagnostic.value = null
  if (!id) {
    optionsState.value = { kind: 'idle' }
    policyState.value = { kind: 'idle' }
    return
  }
  optionsState.value = { kind: 'loading', message: '加载项目 AI 设置…' }
  policyState.value = { kind: 'loading', message: '读取当前策略…' }
  const [optionsResult, policyResult] = await Promise.all([
    getProjectLlmPolicyOptions({ path: { projectId: id } }),
    getActiveProjectLlmPolicy({ path: { projectId: id } }),
  ])
  if (generation !== loadGeneration || selectedProjectId.value !== id) return
  if (optionsResult.error) {
    optionsState.value = { kind: 'error', diagnostic: diagnostic(optionsResult.error, 'PROJECT_LLM_POLICY_OPTIONS_FAILED', '无法读取平台提供的模型选项。') }
    policyState.value = { kind: 'idle' }
    return
  }
  optionsState.value = { kind: 'success', data: optionsResult.data }
  if (policyResult.error) {
    const problem = extractProblemDetails(policyResult.error)
    if (problem?.status === 404 || problem?.diagnosticCode === 'LW_POLICY_NOT_FOUND') {
      policyState.value = { kind: 'empty' }
      syncForm(optionsResult.data, null)
      return
    }
    policyState.value = { kind: 'error', diagnostic: diagnostic(policyResult.error, 'PROJECT_LLM_POLICY_LOAD_FAILED', '无法读取当前项目 AI 设置。') }
    return
  }
  policyState.value = { kind: 'success', data: policyResult.data }
  syncForm(optionsResult.data, policyResult.data)
}

async function savePolicy() {
  const id = selectedProjectId.value
  const project = selectedProject.value
  const options = optionsState.value.kind === 'success' ? optionsState.value.data : null
  const maxCostMicrousd = costMicrousd.value
  const timeoutMillisecondsValue = timeoutMilliseconds.value
  if (!id || !project || !options || maxCostMicrousd === null || timeoutMillisecondsValue === null || !canSave.value) return
  saving.value = true
  saveDiagnostic.value = null
  const revision = activePolicy.value?.revision
  const result = await createProjectLlmPolicy({
    path: { projectId: id },
    headers: {
      'Idempotency-Key': idempotencyKey(),
      ...(revision ? { 'If-Match': ifMatch(revision) } : {}),
    },
    body: {
      id: newUuidV7(),
      projectId: id,
      courseId: project.courseId ?? null,
      revision: revision ?? 1,
      binding: {
        runtimeBinding: options.runtimeBinding,
        model: model.value,
        claudeCodeVersion: options.claudeCodeVersion,
        maxInFlightPerWorker: options.maxInFlightPerWorker,
      },
      budget: { ...budget, maxCostMicrousd, timeoutMilliseconds: timeoutMillisecondsValue },
      deniedDataClasses: [
        'secret',
        'token',
        'private_key',
        'personally_identifiable_information',
        'unallowlisted_student_submission',
      ],
      studentContentMode: 'manifest_allowlist_only',
      activatedAt: new Date().toISOString(),
    },
  })
  saving.value = false
  if (result.error) {
    saveDiagnostic.value = diagnostic(result.error, 'PROJECT_LLM_POLICY_SAVE_FAILED', '项目 AI 设置保存失败，请检查输入后重试。')
    return
  }
  policyState.value = { kind: 'success', data: result.data }
  materialsConsent.value = true
}

function modelLabel(value: string, options: ProjectLlmPolicyOptionsSchema): string {
  return options.models.find((option) => option.model === value)?.label ?? '已配置模型'
}

watch(
  [() => projects.projects, routeProjectId],
  ([state, requestedProjectId]) => {
    if (state.kind !== 'success') return
    const preferred = requestedProjectId
      ? state.data.find((project) => project.id === requestedProjectId)?.id
      : projects.selectedProjectId && state.data.some((project) => project.id === projects.selectedProjectId)
        ? projects.selectedProjectId
        : state.data[0]?.id
    if (preferred && preferred !== projects.selectedProjectId) projects.select(preferred)
  },
  { immediate: true },
)

watch(() => projects.selectedProjectId, (projectId) => {
  if (!projectId || projectContextUnavailable.value || routeProjectId.value === projectId) return
  void router.replace({ query: { ...route.query, projectId } })
})

watch(selectedProjectId, () => {
  void loadPolicyContext()
}, { immediate: true })
</script>

<style scoped>
.policy-page { display: grid; gap: 20px; }
.page-header, .section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.page-header h2, .section-heading h3 { margin: 0; color: var(--md-sys-color-on-surface); }
.page-header h2 { font: var(--md-sys-headline-small); }
.section-heading h3 { font: var(--md-sys-title-large); }
.eyebrow { margin: 0 0 5px; color: var(--md-sys-color-primary); font: var(--md-sys-label-medium); text-transform: uppercase; letter-spacing: .08em; }
.page-subtitle, .section-heading p, .field-help, .form-note { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.project-strip { display: flex; align-items: end; gap: 16px; padding: 16px 20px; }
.project-strip label, .policy-form label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.project-strip label { flex: 1; max-width: 560px; }
.project-scope { padding-bottom: 10px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.project-context-action { justify-self: start; }
.policy-layout { display: grid; grid-template-columns: minmax(280px, .8fr) minmax(0, 1.2fr); gap: 20px; align-items: start; }
.policy-summary, .policy-form { display: grid; gap: 18px; padding: 20px; }
.summary-grid { display: grid; gap: 10px; margin: 0; }
.summary-grid div { display: grid; gap: 5px; padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.summary-grid dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.summary-grid dd { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); overflow-wrap: anywhere; }
.empty-policy, .empty-page { display: grid; justify-items: center; gap: 12px; text-align: center; color: var(--md-sys-color-on-surface-variant); }
.empty-policy { padding: 24px 10px; }
.empty-policy p, .empty-page p { max-width: 520px; margin: 0; line-height: 1.5; }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.scope-fieldset, .budget-fieldset { display: grid; gap: 12px; margin: 0; padding: 14px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); }
.scope-fieldset legend, .budget-fieldset legend { padding: 0 5px; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.authorization-field { display: flex !important; align-items: flex-start; gap: 9px !important; }
.authorization-field input { margin-top: 3px; accent-color: var(--md-sys-color-primary); }
.authorization-field span { color: var(--md-sys-color-on-surface); line-height: 1.5; }
.field-help { font-size: var(--md-sys-body-small); }
.inline-link { color: var(--md-sys-color-primary); font: var(--md-sys-label-large); }
.advanced-settings { display: grid; gap: 10px; padding-top: 4px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.advanced-settings summary { cursor: pointer; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); }
.advanced-settings .field-help { margin: 0; }
.budget-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
.validation-message { margin: 0; color: var(--md-sys-color-error); font: var(--md-sys-body-small); }
.form-actions { display: flex; align-items: center; flex-wrap: wrap; gap: 12px; }
.filled-button, .outlined-button, .icon-button { cursor: pointer; }
.filled-button, .outlined-button { display: inline-flex; align-items: center; justify-content: center; min-height: 40px; padding: 0 17px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-large); text-decoration: none; }
.filled-button { border: 1px solid var(--md-sys-color-primary); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); }
.outlined-button { border: 1px solid var(--md-sys-color-outline); background: transparent; color: var(--md-sys-color-primary); }
.filled-button:disabled, .outlined-button:disabled, .icon-button:disabled { opacity: .5; cursor: not-allowed; }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); }
.state-chip { display: inline-flex; align-self: start; white-space: nowrap; padding: 3px 8px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.state-chip--ready { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
@media (max-width: 850px) { .policy-layout { grid-template-columns: 1fr; } .project-strip { align-items: stretch; flex-direction: column; } .project-strip label { max-width: none; } .project-scope { padding-bottom: 0; } }
@media (max-width: 620px) { .budget-grid { grid-template-columns: 1fr; } .form-actions { align-items: stretch; flex-direction: column; } .form-actions .filled-button { width: 100%; } }
</style>
