<template>
  <div class="review-page">
    <header class="review-header">
      <h2>审核与发布</h2>
      <p>选择待审核版本，查看题面材料、环境、运行权限、评分规则和学生看到的内容，确认后一次批准并发布。</p>
    </header>

    <DiagnosticBanner
      v-if="!projectId"
      code="PROJECT_CONTEXT_REQUIRED"
      message="请先在顶栏项目选择器中选择项目。"
      :retryable="false"
      severity="info"
    />

    <div v-else class="review-layout">
      <ReviewQueue :project-id="projectId" :selected-run-id="runId" @loaded="onQueueLoaded" />

      <main class="review-main" aria-labelledby="review-title">
        <div v-if="!runId" class="review-placeholder md-card" role="status" data-testid="review-placeholder">
          <SvgIcon name="fact_check" size="lg" aria-hidden="true" />
          <template v-if="queueSize === 0">
            <p id="review-title"><strong>还没有可以审核的版本。</strong></p>
            <p>在“创建与生成实验”上传材料并生成候选，生成完成后会出现在这里。</p>
          </template>
          <template v-else>
            <p id="review-title"><strong>从左侧选择一个待审核版本。</strong></p>
            <p>生成失败的任务列在“未完成”中，点击后回到材料页沿原任务继续处理。</p>
          </template>
        </div>

        <template v-else>
          <AsyncStateView :state="authoring.run" @retry="authoring.load">
            <template #success="{ data: run }">
              <div class="review-body">
              <section class="review-summary md-card" aria-labelledby="review-title">
                <div class="summary-head">
                  <div>
                    <h3 id="review-title">{{ experimentTitle }}</h3>
                    <p class="summary-meta">
                      {{ runtimeText }} · {{ runStateLabel(run.state) }}<template v-if="evaluationName"> · 评分规则 {{ evaluationName }}</template>
                    </p>
                  </div>
                  <GcpStatusPill :state="run.state" domain="agent" />
                </div>
                <ol class="readiness" aria-label="发布前检查">
                  <li v-for="item in readiness" :key="item.label" :data-ready="item.ready">
                    <SvgIcon :name="item.ready ? 'check_circle' : 'pending'" size="sm" aria-hidden="true" />
                    <span>{{ item.label }}</span>
                    <small>{{ item.detail }}</small>
                  </li>
                </ol>
              </section>

              <TabsRoot v-model="tab" class="review-tabs">
                <TabsList class="tab-list" aria-label="审核内容">
                  <TabsTrigger v-for="item in tabs" :key="item.value" :value="item.value" class="tab-trigger">
                    {{ item.label }}
                  </TabsTrigger>
                </TabsList>

                <TabsContent value="overview" class="tab-pane">
                  <div class="pane-grid">
                    <section class="pane-card md-card" aria-labelledby="cost-heading">
                      <h4 id="cost-heading">资源与费用影响</h4>
                      <dl v-if="environment" class="meta-grid">
                        <template v-for="row in resourceRows(environment.candidate.spec)" :key="row.label">
                          <dt>{{ row.label }}</dt><dd>{{ row.value }}</dd>
                        </template>
                      </dl>
                      <p class="pane-note">每位学生启动环境时按这个规格申请资源，用量按项目的资源费率计量。平台尚未提供发布前估价，实际费用以资源与费用记录为准。</p>
                    </section>
                    <section class="pane-card md-card" aria-labelledby="history-heading">
                      <h4 id="history-heading">生成过程</h4>
                      <ul class="history-list">
                        <li v-for="track in trackHistories(run)" :key="track.kind">
                          <strong>{{ track.label }}</strong>
                          <span v-for="attempt in track.attempts" :key="attempt.number" class="attempt" :data-state="attempt.state">
                            第 {{ attempt.number }} 次：{{ attemptStateLabel(attempt.state) }}<code v-if="attempt.diagnosticCode">{{ attempt.diagnosticCode }}</code>
                          </span>
                        </li>
                      </ul>
                      <p class="pane-note">审核的是最后一次成功生成的候选。材料或配置变化后会生成新的候选，需要重新审核。</p>
                    </section>
                  </div>
                </TabsContent>

                <TabsContent value="environment" class="tab-pane">
                  <AsyncStateView :state="authoring.environmentCandidate" @retry="authoring.load">
                    <template #success="{ data: view }">
                      <div class="pane-grid">
                        <section class="pane-card md-card" aria-labelledby="env-heading">
                          <h4 id="env-heading">环境</h4>
                          <dl class="meta-grid">
                            <dt>名称</dt><dd>{{ view.candidate.spec.name }}</dd>
                            <dt>运行方式</dt><dd>{{ runtimeLabel(view.candidate.spec.runtime.kind) }}</dd>
                            <dt>学生入口</dt><dd>{{ entryLabels(view.candidate.spec).join('、') || '无' }}</dd>
                            <template v-for="row in resourceRows(view.candidate.spec)" :key="row.label">
                              <dt>{{ row.label }}</dt><dd>{{ row.value }}</dd>
                            </template>
                            <dt>镜像构建</dt><dd>{{ buildStateLabel(view.build?.state) }}</dd>
                          </dl>
                          <CandidateBuildTask
                            v-if="view.build?.state === 'requested'"
                            :project-id="view.candidate.projectId"
                            :candidate-id="view.candidate.id"
                            target="environment"
                          />
                          <DiagnosticBanner
                            v-if="view.build?.diagnosticCode"
                            :code="view.build.diagnosticCode"
                            :message="`镜像${buildStateLabel(view.build.state)}`"
                            :retryable="false"
                            severity="warning"
                          />
                        </section>
                        <section class="pane-card md-card" aria-labelledby="perm-heading">
                          <h4 id="perm-heading">运行权限</h4>
                          <dl class="meta-grid">
                            <template v-for="row in permissionRows(view.candidate.spec)" :key="row.label">
                              <dt>{{ row.label }}</dt>
                              <dd>{{ row.value }}<small v-if="row.hint" class="row-hint">{{ row.hint }}</small></dd>
                            </template>
                          </dl>
                          <p class="pane-note" data-testid="permission-gap">平台目前只返回上面这些安全策略，还没有特权模式、capability 列表和管理员允许范围，因此这里无法逐项显示。</p>
                          <p class="pane-note">权限随环境、镜像和评分规则一起绑定到发布版本。构建成功不代表已获得运行权限，学生启动时平台仍会按管理员策略检查。</p>
                        </section>
                      </div>
                    </template>
                  </AsyncStateView>
                </TabsContent>

                <TabsContent value="scoring" class="tab-pane">
                  <AsyncStateView :state="authoring.evaluationCandidate" @retry="authoring.load">
                    <template #success="{ data: view }">
                      <section class="pane-card md-card" aria-labelledby="score-heading">
                        <h4 id="score-heading">评分规则 · 满分 {{ view.candidate.spec.spec.aggregation.maxScore }} 分</h4>
                        <ol class="step-list">
                          <li v-for="step in stepSummaries(view.candidate.spec.spec)" :key="step.id" class="step" :data-role="step.role">
                            <div class="step-head">
                              <span class="role-chip">{{ step.roleLabel }}</span>
                              <strong>{{ step.id }}</strong>
                              <span class="step-points">{{ step.points }}</span>
                            </div>
                            <p>{{ step.method }} · {{ step.onFailure }}</p>
                            <ul v-if="step.details.length" class="step-details">
                              <li v-for="line in step.details" :key="line">{{ line }}</li>
                            </ul>
                          </li>
                        </ol>
                      </section>
                      <section class="pane-card md-card" aria-labelledby="submission-heading">
                        <h4 id="submission-heading">学生提交</h4>
                        <dl class="meta-grid">
                          <template v-for="row in submissionRows(view.candidate.spec.spec)" :key="row.label">
                            <dt>{{ row.label }}</dt><dd>{{ row.value }}</dd>
                          </template>
                          <dt>评测镜像</dt>
                          <dd>{{ view.runnerImageArtifact ? '已构建' : authoring.runnerArtifactRequired ? buildStateLabel(view.runnerBuild?.state) : '使用平台自带评测环境' }}</dd>
                        </dl>
                        <CandidateBuildTask
                          v-if="view.runnerBuild?.state === 'requested'"
                          :project-id="view.candidate.projectId"
                          :candidate-id="view.candidate.id"
                          target="evaluation_runner"
                        />
                      </section>
                    </template>
                  </AsyncStateView>
                </TabsContent>

                <TabsContent value="materials" class="tab-pane">
                  <AsyncStateView :state="authoring.problemPackage" @retry="authoring.load">
                    <template #success="{ data: pkg }">
                      <section class="pane-card md-card" aria-labelledby="materials-heading">
                        <h4 id="materials-heading">材料包 · {{ pkg.files.length }} 个文件</h4>
                        <p class="pane-note">上传于 {{ formatTimestamp(pkg.completedAt) }}。教师材料不会出现在学生环境中，学生能看到的内容见“学生预览”。</p>
                        <ul class="file-list">
                          <li v-for="file in pkg.files" :key="file.path">
                            <code>{{ file.path }}</code>
                            <span>{{ formatBytes(file.object.sizeBytes) }}</span>
                          </li>
                        </ul>
                      </section>
                    </template>
                  </AsyncStateView>
                </TabsContent>

                <TabsContent value="preview" class="tab-pane">
                  <section v-if="preview" class="student-preview md-card" aria-labelledby="preview-heading" data-testid="student-preview">
                    <p class="preview-badge">学生视角预览</p>
                    <h4 id="preview-heading">{{ preview.title }}</h4>
                    <p class="preview-meta">{{ preview.runtime }} · {{ preview.entries.join('、') }}</p>
                    <dl class="meta-grid">
                      <template v-for="row in preview.resources" :key="row.label">
                        <dt>{{ row.label }}</dt><dd>{{ row.value }}</dd>
                      </template>
                      <dt>网络</dt><dd>{{ preview.network }}</dd>
                      <template v-for="row in preview.collects" :key="row.label">
                        <dt>{{ row.label }}</dt><dd>{{ row.value }}</dd>
                      </template>
                      <template v-if="preview.requiredFiles.length">
                        <dt>必须提交</dt><dd>{{ preview.requiredFiles.join('、') }}</dd>
                      </template>
                    </dl>
                    <h5>评分方式 · 满分 {{ preview.maxScore }} 分</h5>
                    <ul class="preview-score">
                      <li v-if="preview.gateCount">提交前先做 {{ preview.gateCount }} 项检查，不通过不计分</li>
                      <li v-for="item in preview.scoreItems" :key="item.label">{{ item.label }}：最多 {{ item.points }} 分</li>
                      <li v-if="preview.hasAdvisory">另有 AI 评语，不影响分数</li>
                    </ul>
                    <p class="pane-note">预览只包含学生可见的字段：不含测试代码、测试组、检查参数和教师材料。批准后学生看到的就是这个版本。</p>
                  </section>
                  <p v-else class="pane-note" role="status">环境和评分规则都加载完成后才能预览。</p>
                </TabsContent>

                <TabsContent value="technical" class="tab-pane">
                  <section class="pane-card md-card" aria-labelledby="tech-heading">
                    <h4 id="tech-heading">技术详情</h4>
                    <dl class="meta-grid">
                      <dt>生成任务</dt><dd><code>{{ run.id }}</code> · rev-{{ run.revision }}</dd>
                      <dt>材料包</dt><dd><code>{{ run.packageId }}</code><template v-if="pkgData"> · rev-{{ pkgData.revision }}</template></dd>
                      <template v-if="environment">
                        <dt>环境候选</dt><dd><code>{{ environment.candidate.id }}</code> · rev-{{ environment.candidate.revision }} · 模型 {{ environment.candidate.model }}</dd>
                      </template>
                      <template v-if="evaluation">
                        <dt>评分候选</dt><dd><code>{{ evaluation.candidate.id }}</code> · rev-{{ evaluation.candidate.revision }} · 模型 {{ evaluation.candidate.model }}</dd>
                      </template>
                      <template v-for="entry in boundArtifacts" :key="entry.label">
                        <dt>{{ entry.label }}</dt><dd><code>{{ entry.value }}</code></dd>
                      </template>
                    </dl>
                    <details v-if="environment" class="spec-details"><summary>环境配置 JSON</summary><pre>{{ formatJson(environment.candidate.spec) }}</pre></details>
                    <details v-if="evaluation" class="spec-details"><summary>评分规则 JSON</summary><pre>{{ formatJson(evaluation.candidate.spec) }}</pre></details>
                  </section>
                </TabsContent>
              </TabsRoot>

              <section class="approve-card md-card" aria-labelledby="approve-heading" data-testid="approve-card">
                <h3 id="approve-heading">批准并发布</h3>
                <DiagnosticBanner
                  v-if="authoring.approval.kind === 'error'"
                  :code="authoring.approval.diagnostic.code"
                  :message="authoring.approval.diagnostic.message"
                  :retryable="authoring.approval.diagnostic.retryable"
                  severity="error"
                  @retry="completeApproval"
                />
                <p v-if="authoring.approval.kind === 'error' && diagnosticAction(authoring.approval.diagnostic.code)" class="diagnostic-action" role="status">
                  建议：{{ diagnosticAction(authoring.approval.diagnostic.code) }}
                </p>

                <template v-if="authoring.approval.kind !== 'success'">
                  <p class="pane-note">一次批准绑定这一版的材料包、环境、运行权限、镜像和评分规则，并自动发布。重复提交只会产生一个版本；之后材料或配置有变化，会成为新的候选。</p>
                  <label class="confirm-row">
                    <input v-model="ownerConfirmed" type="checkbox">
                    <span>我已查看环境、运行权限、评分规则和学生预览。</span>
                  </label>
                  <label class="reason-field">
                    <span>批准说明（必填，最多 500 字）</span>
                    <textarea v-model="reason" rows="2" maxlength="500" placeholder="例如：已核对题面与评分标准" />
                  </label>
                  <div class="approve-actions">
                    <button type="button" class="filled-button" :disabled="!canSubmit" data-testid="approve-publish" @click="completeApproval">
                      {{ authoring.acting ? '正在提交…' : '批准并发布' }}
                    </button>
                    <span v-if="!authoring.canApprove" class="pane-note" role="status">{{ blockingReason }}</span>
                  </div>
                </template>

                <div v-else class="approved" role="status">
                  <SvgIcon name="check_circle" size="md" aria-hidden="true" />
                  <strong>已批准，正在发布</strong>
                </div>

                <AsyncStateView v-if="authoring.publication.kind !== 'idle'" :state="authoring.publication" @retry="authoring.loadPublication">
                  <template #success="{ data }">
                    <div class="publication" :data-status="data.status" role="status">
                      <span class="state-chip" :class="`state-chip--${data.status}`">{{ publicationStateLabel(data.status) }}</span>
                      <span v-if="data.status === 'ready'">已发布。可以在“实验总览”中为课程配置这个实验。</span>
                      <span v-else-if="data.status === 'failed'">发布未完成<code v-if="data.diagnosticCode">{{ data.diagnosticCode }}</code>。处理后点击重试，会沿用同一次批准。</span>
                      <span v-else>正在发布，页面会自动更新。</span>
                    </div>
                  </template>
                </AsyncStateView>
              </section>
              </div>
            </template>
          </AsyncStateView>
          <p v-if="stateGuidance(authoring.run)" class="diagnostic-action" role="status">建议：{{ stateGuidance(authoring.run) }}</p>
        </template>
      </main>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { TabsContent, TabsList, TabsRoot, TabsTrigger } from 'reka-ui'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import CandidateBuildTask from '@/components/common/CandidateBuildTask.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import GcpStatusPill from '@/components/common/GcpStatusPill.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import ReviewQueue from '@/components/review/ReviewQueue.vue'
import { useProjects } from '@/composables/useProjects'
import { useProjectAuthoringApproval } from '@/composables/useProjectAuthoringApproval'
import type { AgentRunHistoryItem, AuthoringPublicationState, CompleteAuthoringApprovalRequestSchemaImageArtifact } from '@/generated/contracts'
import {
  attemptStateLabel,
  buildStateLabel,
  entryLabels,
  permissionRows,
  resourceRows,
  runStateLabel,
  runtimeLabel,
  stepSummaries,
  studentPreview,
  submissionRows,
  trackHistories,
} from '@/utils/authoringReview'
import { formatBytes, formatTimestamp } from '@/utils/format'

const route = useRoute()
const router = useRouter()
const projects = useProjects()

const queryString = (value: unknown) => typeof value === 'string' && value.trim() ? value.trim() : undefined
const runId = computed(() => queryString(route.query.runId))
const approvalId = computed(() => queryString(route.query.approvalId))
const projectId = computed(() => projects.selectedProjectId ?? null)
const authoring = useProjectAuthoringApproval(projectId, runId, approvalId)

watch(
  () => projects.projects,
  (state) => {
    if (state.kind !== 'success') return
    const requested = queryString(route.query.projectId)
    if (requested && state.data.some((project) => project.id === requested)) projects.select(requested)
  },
  { immediate: true },
)

/** Open the first reviewable version when the page is entered without one. */
const queueSize = ref<number | null>(null)
function onQueueLoaded(items: AgentRunHistoryItem[]) {
  queueSize.value = items.length
  if (runId.value) return
  const first = items.find((item) => item.state === 'awaiting_approval') ?? items.find((item) => item.state === 'succeeded' || item.state === 'partially_succeeded')
  if (first) void router.replace({ query: { ...route.query, projectId: projectId.value ?? undefined, runId: first.id } })
}

watch(
  () => authoring.approval.kind === 'success' ? authoring.approval.data.id : undefined,
  (id) => {
    if (!id || approvalId.value === id) return
    void router.replace({ query: { ...route.query, approvalId: id } })
  },
)

const tabs = [
  { value: 'overview', label: '概览' },
  { value: 'environment', label: '环境与权限' },
  { value: 'scoring', label: '评分规则' },
  { value: 'materials', label: '材料' },
  { value: 'preview', label: '学生预览' },
  { value: 'technical', label: '技术详情' },
]
const tab = ref('overview')
const reason = ref('')
const ownerConfirmed = ref(false)
watch(runId, () => {
  tab.value = 'overview'
  reason.value = ''
  ownerConfirmed.value = false
})

const environment = computed(() => authoring.environmentCandidate.kind === 'success' ? authoring.environmentCandidate.data : null)
const evaluation = computed(() => authoring.evaluationCandidate.kind === 'success' ? authoring.evaluationCandidate.data : null)
const pkgData = computed(() => authoring.problemPackage.kind === 'success' ? authoring.problemPackage.data : null)

const experimentTitle = computed(() => environment.value?.candidate.spec.name ?? '实验候选')
const evaluationName = computed(() => evaluation.value ? `${evaluation.value.candidate.spec.metadata.name} ${evaluation.value.candidate.spec.metadata.version}` : '')
const runtimeText = computed(() => environment.value ? runtimeLabel(environment.value.candidate.spec.runtime.kind) : '运行方式待确认')
const preview = computed(() => environment.value && evaluation.value ? studentPreview(environment.value.candidate.spec, evaluation.value.candidate.spec.spec) : null)

const readiness = computed(() => {
  const env = environment.value
  const evalView = evaluation.value
  const runnerReady = !authoring.runnerArtifactRequired || Boolean(evalView?.runnerImageArtifact)
  return [
    { label: '材料包', ready: Boolean(pkgData.value), detail: pkgData.value ? `${pkgData.value.files.length} 个文件` : '读取中' },
    { label: '环境', ready: Boolean(env?.imageArtifact), detail: env ? buildStateLabel(env.build?.state) : '读取中' },
    { label: '评分规则', ready: Boolean(evalView) && runnerReady, detail: evalView ? `${evalView.candidate.spec.spec.steps.length} 个步骤 · ${runnerReady ? '评测环境就绪' : buildStateLabel(evalView.runnerBuild?.state)}` : '读取中' },
    { label: '批准', ready: authoring.approval.kind === 'success', detail: authoring.approval.kind === 'success' ? '已批准' : '等待批准' },
  ]
})

const blockingReason = computed(() => {
  const missing = readiness.value.slice(0, 3).filter((item) => !item.ready).map((item) => item.label)
  return missing.length ? `${missing.join('、')}尚未就绪，就绪后才能批准。` : '生成任务尚未完成，暂时不能批准。'
})

const canSubmit = computed(() => {
  const length = reason.value.trim().length
  return ownerConfirmed.value && length >= 1 && length <= 500 && authoring.canApprove && !authoring.acting
})

async function completeApproval() {
  if (!canSubmit.value) return
  await authoring.complete(reason.value)
}

const boundArtifacts = computed(() => [
  ...(authoring.imageArtifact ? artifactEntries('环境镜像', authoring.imageArtifact) : []),
  ...(authoring.evaluationRunnerImageArtifact ? artifactEntries('评测镜像', authoring.evaluationRunnerImageArtifact) : []),
])

function artifactEntries(prefix: string, artifact: CompleteAuthoringApprovalRequestSchemaImageArtifact) {
  if (artifact.kind === 'container') {
    return [{ label: prefix, value: `${artifact.repository}@${artifact.digest}` }]
  }
  return [{ label: prefix, value: `${artifact.format} · ${artifact.base_disk.binding} · ${artifact.base_disk.sourceRegistryDigest}` }]
}

function diagnosticAction(code: string): string | null {
  switch (code) {
    case 'LW_RESOURCE_EXHAUSTED':
      return 'AI 生成预算可能已达项目上限。请缩短材料后重试，或请管理员检查项目 AI 预算与调用限制。'
    case 'REVISION_CONFLICT':
      return '审核内容已经变化，请刷新后确认最新候选和材料包。'
    case 'LW_ACCESS_DENIED':
      return '请确认当前账号仍有该项目的教师权限。'
    case 'LW_CANDIDATE_NOT_FOUND':
      return '候选可能仍在同步，点击重试继续读取。'
    case 'PROJECT_RUN_STALE_CONTEXT':
    case 'PROJECT_APPROVAL_STALE_CONTEXT':
    case 'UPLOAD_RUN_PACKAGE_MISMATCH':
      return '链接引用了其他项目或材料，请从左侧列表重新选择。'
    default:
      return null
  }
}

type DiagnosticState = { kind: string; diagnostic?: { code: string } }
function stateGuidance(state: DiagnosticState): string | null {
  if (!['error', 'blocked', 'timeout', 'conflict', 'unauthorized', 'revoked'].includes(state.kind)) return null
  return state.diagnostic ? diagnosticAction(state.diagnostic.code) : null
}

function formatJson(value: unknown): string {
  return JSON.stringify(value, null, 2)
}

function publicationStateLabel(state: AuthoringPublicationState): string {
  return ({ pending: '等待发布', publishing: '发布中', ready: '已发布', failed: '发布失败' } as Record<AuthoringPublicationState, string>)[state]
}
</script>

<style scoped>
.review-page { display: flex; flex-direction: column; gap: 20px; }
.review-header h2 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-headline-small); }
.review-header p { margin: 4px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.review-layout { display: grid; grid-template-columns: minmax(240px, 300px) minmax(0, 1fr); gap: 20px; align-items: start; }
.review-main { display: grid; gap: 16px; min-width: 0; }
.review-body { display: grid; gap: 16px; }
.md-card { border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); background: var(--md-sys-color-surface-container-low); }
.review-placeholder { display: grid; justify-items: start; gap: 6px; padding: 24px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.review-placeholder p { margin: 0; }
.review-summary { display: grid; gap: 14px; padding: 16px 20px; }
.summary-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; }
.summary-head h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-large); }
.summary-meta { margin: 4px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.readiness { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 8px; margin: 0; padding: 0; list-style: none; }
.readiness li { display: grid; grid-template-columns: auto 1fr; gap: 0 8px; align-items: center; padding: 10px 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-large); }
.readiness li small { grid-column: 2; font: var(--md-sys-body-small); }
.readiness li[data-ready='true'] { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.review-tabs { display: grid; gap: 12px; }
.tab-list { display: flex; flex-wrap: wrap; gap: 4px; border-bottom: 1px solid var(--md-sys-color-outline-variant); }
.tab-trigger { min-height: 44px; padding: 0 16px; border: 0; border-bottom: 3px solid transparent; background: transparent; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-title-small); cursor: pointer; }
.tab-trigger[data-state='active'] { border-bottom-color: var(--md-sys-color-primary); color: var(--md-sys-color-primary); }
.tab-trigger:focus-visible { outline: 2px solid var(--md-sys-color-primary); outline-offset: -2px; }
.tab-pane { display: grid; gap: 12px; }
.tab-pane[hidden] { display: none; }
.tab-pane:focus-visible { outline: 2px solid var(--md-sys-color-primary); outline-offset: 2px; }
.pane-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
.pane-card { display: grid; align-content: start; gap: 10px; padding: 16px; min-width: 0; }
.pane-card h4, .student-preview h4 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.pane-note { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.meta-grid { display: grid; grid-template-columns: minmax(96px, 140px) minmax(0, 1fr); gap: 8px 16px; margin: 0; }
.meta-grid dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.meta-grid dd { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); overflow-wrap: anywhere; }
.row-hint { display: block; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.history-list { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; font: var(--md-sys-body-medium); }
.history-list li { display: flex; flex-wrap: wrap; gap: 4px 12px; align-items: baseline; }
.attempt { color: var(--md-sys-color-on-surface-variant); }
.attempt[data-state='failed'] { color: var(--md-sys-color-error); }
.attempt code { margin-left: 4px; font: var(--md-sys-label-medium); }
.step-list { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.step { display: grid; gap: 4px; padding: 10px 12px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); }
.step p { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.step-head { display: flex; align-items: center; gap: 8px; }
.step-points { margin-left: auto; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.role-chip { padding: 2px 8px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-container-high); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.step[data-role='score'] .role-chip { background: var(--md-sys-color-primary-container); color: var(--md-sys-color-on-primary-container); }
.step-details { margin: 0; padding-left: 18px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.file-list { display: grid; gap: 4px; margin: 0; padding: 0; list-style: none; font: var(--md-sys-body-medium); }
.file-list li { display: flex; justify-content: space-between; gap: 12px; }
.file-list span { color: var(--md-sys-color-on-surface-variant); }
.student-preview { display: grid; gap: 10px; max-width: 720px; padding: 20px; border-style: dashed; background: var(--md-sys-color-surface); }
.student-preview h5 { margin: 6px 0 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.preview-badge { justify-self: start; margin: 0; padding: 2px 10px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); font: var(--md-sys-label-medium); }
.preview-meta { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.preview-score { margin: 0; padding-left: 18px; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.spec-details summary { color: var(--md-sys-color-primary); cursor: pointer; font: var(--md-sys-label-large); }
.spec-details pre { max-height: 320px; overflow: auto; padding: 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container); font: var(--md-sys-body-small); }
.approve-card { display: grid; gap: 10px; padding: 16px 20px; }
.approve-card h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.confirm-row { display: flex; gap: 8px; align-items: flex-start; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.reason-field { display: grid; gap: 4px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.reason-field textarea { padding: 8px 12px; border: 1px solid var(--md-sys-color-outline); border-radius: var(--md-sys-shape-extra-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); resize: vertical; }
.approve-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 16px; }
.filled-button { min-height: 40px; padding: 0 24px; border: 0; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); font: var(--md-sys-label-large); cursor: pointer; }
.filled-button:disabled { background: color-mix(in srgb, var(--md-sys-color-on-surface) 12%, transparent); color: color-mix(in srgb, var(--md-sys-color-on-surface) 38%, transparent); cursor: not-allowed; }
.approved { display: flex; align-items: center; gap: 8px; color: var(--md-sys-color-primary); font: var(--md-sys-title-small); }
.publication { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; font: var(--md-sys-body-medium); }
.publication code { margin-left: 4px; }
.state-chip { padding: 2px 10px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-container-high); font: var(--md-sys-label-medium); }
.state-chip--ready { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--failed { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
.diagnostic-action { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
@media (max-width: 1100px) {
  .review-layout { grid-template-columns: 1fr; }
  .readiness { grid-template-columns: repeat(2, minmax(0, 1fr)); }
}
@media (max-width: 720px) {
  .pane-grid { grid-template-columns: 1fr; }
  .meta-grid { grid-template-columns: 1fr; gap: 2px; }
  .meta-grid dd { margin-bottom: 6px; }
}
</style>
