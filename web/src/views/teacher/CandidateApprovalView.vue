<template>
  <div class="review-page">
    <header class="review-header">
      <h2>审核与发布</h2>
      <p>选择待审核版本，查看题面材料、环境、运行权限和评分规则，确认后一次批准并发布。</p>
    </header>

    <DiagnosticBanner
      v-if="!projectId"
      code="PROJECT_CONTEXT_REQUIRED"
      message="请先在顶栏项目选择器中选择项目。"
      :retryable="false"
      severity="info"
    />

    <div v-else class="review-layout">
      <ReviewQueue ref="queue" :project-id="projectId" :selected-run-id="runId" @loaded="onQueueLoaded" />

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
                          <p class="pane-note" data-testid="permission-gap">普通容器支持 root 和受控 capability；此页尚未逐项展示 capability 与管理员允许范围。特权容器和 DinD 暂不支持。</p>
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
                        <p class="pane-note">上传于 {{ formatTimestamp(pkg.completedAt) }}。教师材料不会复制到学生环境中。</p>
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
                      <template v-if="authoring.approval.kind === 'success'">
                        <dt>批准记录</dt><dd><code>{{ authoring.approval.data.id }}</code></dd>
                      </template>
                      <template v-if="publicationData?.environmentReleaseId">
                        <dt>环境发布</dt><dd><code>{{ publicationData.environmentReleaseId }}</code></dd>
                      </template>
                      <template v-if="publicationData?.evaluationReleaseId">
                        <dt>评分规则发布</dt><dd><code>{{ publicationData.evaluationReleaseId }}</code></dd>
                      </template>
                      <template v-if="runRelease">
                        <dt>已发布版本</dt><dd><code>{{ runRelease.id }}</code> · 版本 {{ runRelease.version }} · 候选 rev-{{ runRelease.candidateRevision }}</dd>
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
                  :request-id="authoring.approval.diagnostic.requestId"
                  :next-step="authoring.approval.diagnostic.nextStep"
                  :message="authoring.approval.diagnostic.message"
                  :retryable="authoring.approval.diagnostic.retryable"
                  severity="error"
                  @retry="completeApproval"
                />
                <p v-if="authoring.approval.kind === 'error' && diagnosticAction(authoring.approval.diagnostic.code)" class="diagnostic-action" role="status">
                  建议：{{ diagnosticAction(authoring.approval.diagnostic.code) }}
                </p>
                <DiagnosticBanner
                  v-if="authoring.rejectedApproval"
                  :code="authoring.rejectedApproval.code"
                  :message="authoring.rejectedApproval.message"
                  :retryable="false"
                  severity="warning"
                  data-testid="approval-mismatch"
                />
                <DiagnosticBanner
                  v-if="authoring.releaseApprovalRecord.kind === 'error'"
                  :code="authoring.releaseApprovalRecord.diagnostic.code"
                  :request-id="authoring.releaseApprovalRecord.diagnostic.requestId"
                  :next-step="authoring.releaseApprovalRecord.diagnostic.nextStep"
                  :message="authoring.releaseApprovalRecord.diagnostic.message"
                  :retryable="true"
                  severity="error"
                  data-testid="release-verify-error"
                  @retry="authoring.loadReleaseApproval(runRelease?.approval?.id ?? null)"
                />

                <div v-if="publishedRelease && authoring.approval.kind !== 'success'" class="approved" role="status" data-testid="review-published">
                  <SvgIcon name="check_circle" size="md" aria-hidden="true" />
                  <span>
                    <strong>这一版已发布为版本 {{ publishedRelease.version }}</strong>（{{ formatTimestamp(publishedRelease.publishedAt) }}）。
                    可以在<RouterLink :to="{ path: '/teacher/labs', query: { projectId } }">已发布环境模板</RouterLink>中查看，不需要再次批准。
                  </span>
                </div>

                <template v-else-if="authoring.approval.kind !== 'success'">
                  <p v-if="supersededRelease" class="pane-note" role="status" data-testid="review-superseded">
                    这个任务之前发布过版本 {{ supersededRelease.version }}，之后材料包、环境或评分规则有变化。当前版本需要重新审核，批准后会发布为新版本，已发布的版本不受影响。
                  </p>
                  <p class="pane-note">一次批准绑定这一版的材料包、环境、运行权限、镜像和评分规则，并自动发布。网络失败后重试或重复点击沿用同一次提交，只产生一个版本；之后材料或配置有变化，会成为新的候选。</p>
                  <label class="confirm-row">
                    <input v-model="ownerConfirmed" type="checkbox">
                    <span>我已查看材料、环境、运行权限和评分规则。</span>
                  </label>
                  <label class="reason-field">
                    <span>批准说明（必填，最多 500 字）</span>
                    <textarea v-model="reason" rows="2" maxlength="500" placeholder="例如：已核对题面与评分标准" />
                  </label>
                  <div class="approve-actions">
                    <button type="button" class="filled-button" :disabled="!canSubmit" data-testid="approve-publish" @click="completeApproval">
                      {{ authoring.acting ? '正在提交…' : '批准并发布' }}
                    </button>
                    <span v-if="!approvalAllowed" class="pane-note" role="status" data-testid="approve-blocked">{{ blockingReason }}</span>
                  </div>
                </template>

                <div v-else class="approved" role="status">
                  <SvgIcon name="check_circle" size="md" aria-hidden="true" />
                  <strong>已批准</strong>
                </div>

                <AsyncStateView v-if="authoring.publication.kind !== 'idle'" :state="authoring.publication" @retry="authoring.loadPublication">
                  <template #success="{ data }">
                    <div class="publication" :data-status="data.status" role="status" data-testid="publication-status">
                      <span class="state-chip" :class="`state-chip--${data.status}`">{{ publicationStateLabel(data.status) }}</span>
                      <span v-if="data.status === 'ready'">
                        已发布。可以在<RouterLink :to="{ path: '/teacher/labs', query: { projectId } }">已发布环境模板</RouterLink>中查看。
                      </span>
                      <span v-else-if="data.status !== 'failed'">正在发布，页面会自动更新。</span>
                    </div>
                    <DiagnosticBanner
                      v-if="data.status === 'failed'"
                      :code="data.diagnosticCode ?? 'PROJECT_AUTHORING_PUBLICATION_FAILED'"
                      message="批准已记录，但发布没有完成。请把诊断信息发给平台管理员处理。"
                      :retryable="false"
                      severity="error"
                    />
                    <RubricModelInputRulesPanel
                      v-if="data.status === 'ready' && projectId && data.evaluationReleaseId && data.evaluationReleaseRevision && pkgData && pkgData.id === data.approval.packageId && pkgData.revision === data.approval.packageRevision && advisoryRubricSteps.length > 0"
                      :project-id="projectId"
                      :evaluation-release-id="data.evaluationReleaseId"
                      :evaluation-release-revision="data.evaluationReleaseRevision"
                      :problem-package="pkgData"
                      :advisory-steps="advisoryRubricSteps"
                    />
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
import RubricModelInputRulesPanel from '@/components/review/RubricModelInputRulesPanel.vue'
import { useProjects } from '@/composables/useProjects'
import { useProjectAuthoringApproval } from '@/composables/useProjectAuthoringApproval'
import type { AuthoringPublicationState, CompleteAuthoringApprovalRequestSchemaImageArtifact } from '@/generated/contracts'
import {
  attemptStateLabel,
  buildStateLabel,
  entryLabels,
  firstReviewableRun,
  permissionRows,
  releaseMatchesCandidate,
  resourceRows,
  runStateLabel,
  runtimeLabel,
  stepSummaries,
  submissionRows,
  trackHistories,
  type ReviewQueueSnapshot,
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

/** Open the first reviewable version without a release when the page is entered without one. */
const queue = ref<InstanceType<typeof ReviewQueue> | null>(null)
const queueSnapshot = ref<ReviewQueueSnapshot | null>(null)
const queueSize = computed(() => queueSnapshot.value?.items.length ?? null)
function onQueueLoaded(snapshot: ReviewQueueSnapshot) {
  queueSnapshot.value = snapshot
  if (runId.value) return
  const first = firstReviewableRun(snapshot)
  if (first) void router.replace({ query: { ...route.query, projectId: projectId.value ?? undefined, runId: first.id } })
}

/** A finished publication moves the version to “已发布” in the queue. */
watch(
  () => authoring.publication.kind === 'success' ? authoring.publication.data.status : undefined,
  (status, previous) => {
    if (status === 'ready' && previous !== 'ready') void queue.value?.load()
  },
)

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
const publicationData = computed(() => authoring.publication.kind === 'success' ? authoring.publication.data : null)
/** Rubric files the published Evaluation release hands to its AI review steps. */
const advisoryRubricSteps = computed(() => (evaluation.value?.candidate.spec.spec.steps ?? [])
  .flatMap((step) => (step.role === 'advisory' ? [{ id: step.id, rubric: step.runner.rubric }] : [])))

const experimentTitle = computed(() => environment.value?.candidate.spec.name ?? '实验候选')
const evaluationName = computed(() => evaluation.value ? `${evaluation.value.candidate.spec.metadata.name} ${evaluation.value.candidate.spec.metadata.version}` : '')
const runtimeText = computed(() => environment.value ? runtimeLabel(environment.value.candidate.spec.runtime.kind) : '运行方式待确认')

/** Release list of the current project, or null while it is unknown (not read yet or read failed). */
const projectReleases = computed(() => {
  const snapshot = queueSnapshot.value
  return snapshot && snapshot.projectId === projectId.value && !snapshot.releaseError ? snapshot.releasesByRun : null
})
/** Active release of this run, read from the server's release list. */
const runRelease = computed(() => (runId.value && projectReleases.value?.get(runId.value)) || null)

/** Read the complete approval behind the run's release; it binds package and both candidates. */
watch(
  () => [runRelease.value?.approval?.id ?? null, runId.value, projectId.value] as const,
  ([releaseApprovalId]) => void authoring.loadReleaseApproval(releaseApprovalId),
  { immediate: true },
)

/**
 * Whether this version is already published. The Environment candidate alone
 * is not enough: a retried Evaluation track or a new package revision keeps the
 * Environment candidate but needs a new approval. Unknown states fail closed.
 */
const releaseState = computed<'none' | 'pending' | 'unknown' | 'published' | 'superseded'>(() => {
  const release = runRelease.value
  if (!release) return 'none'
  if (!environment.value) return 'pending'
  if (!releaseMatchesCandidate(release, environment.value.candidate)) return 'superseded'
  switch (authoring.releaseBinding) {
    case 'match': return 'published'
    case 'mismatch': return 'superseded'
    case 'unknown': return 'unknown'
    default: return 'pending'
  }
})
/** The release publishes exactly the version under review: nothing left to approve. */
const publishedRelease = computed(() => (releaseState.value === 'published' ? runRelease.value : null))
/** An earlier version of this run was published; the current one needs its own approval. */
const supersededRelease = computed(() => (releaseState.value === 'superseded' ? runRelease.value : null))
const approvalDetail = computed(() => {
  if (authoring.approval.kind === 'success') return '已批准'
  if (publishedRelease.value) return `已发布版本 ${publishedRelease.value.version}`
  return '等待批准'
})

const readiness = computed(() => {
  const env = environment.value
  const evalView = evaluation.value
  const runnerReady = !authoring.runnerArtifactRequired || Boolean(evalView?.runnerImageArtifact)
  return [
    { label: '材料包', ready: Boolean(pkgData.value), detail: pkgData.value ? `${pkgData.value.files.length} 个文件` : '读取中' },
    { label: '环境', ready: Boolean(env?.imageArtifact), detail: env ? buildStateLabel(env.build?.state) : '读取中' },
    { label: '评分规则', ready: Boolean(evalView) && runnerReady, detail: evalView ? `${evalView.candidate.spec.spec.steps.length} 个步骤 · ${runnerReady ? '评测环境就绪' : buildStateLabel(evalView.runnerBuild?.state)}` : '读取中' },
    { label: '批准', ready: authoring.approval.kind === 'success' || Boolean(publishedRelease.value), detail: approvalDetail.value },
  ]
})

const blockingReason = computed(() => {
  if (!projectReleases.value) {
    return queueSnapshot.value?.releaseError
      ? '没能读取已发布版本，无法确认这一版是否已经发布。请刷新左侧列表后再批准。'
      : '正在读取已发布版本，读取完成后才能批准。'
  }
  const missing = readiness.value.slice(0, 3).filter((item) => !item.ready).map((item) => item.label)
  if (missing.length) return `${missing.join('、')}尚未就绪，就绪后才能批准。`
  if (releaseState.value === 'unknown') return '没能核对已发布版本的批准记录，无法确认这一版是否已经发布。重试成功后才能批准。'
  if (releaseState.value === 'pending') return '正在核对已发布版本，核对完成后才能批准。'
  if (approvalId.value && !authoring.rejectedApproval) return '正在核对链接中的批准记录，核对完成前不能再次批准。'
  return '生成任务尚未完成，暂时不能批准。'
})

/** Approval needs a known release state: an unreadable or unverified release fails closed. */
const approvalAllowed = computed(() => (
  authoring.canApprove
  && projectReleases.value !== null
  && (releaseState.value === 'none' || releaseState.value === 'superseded')
))

const canSubmit = computed(() => {
  const length = reason.value.trim().length
  return ownerConfirmed.value && length >= 1 && length <= 500 && approvalAllowed.value && !authoring.acting
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
  if (artifact.kind === 'container' || artifact.kind === 'catalog_container') {
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
.review-page { display: flex; flex-direction: column; gap: var(--space-5); }
.review-header h2 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-headline-small); }
.review-header p { margin: var(--space-1) 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.review-layout { display: grid; grid-template-columns: minmax(240px, 300px) minmax(0, 1fr); gap: var(--space-5); align-items: start; }
.review-main { display: grid; gap: var(--space-4); min-width: 0; }
.review-body { display: grid; gap: var(--space-4); }
.md-card { border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); background: var(--md-sys-color-surface-container-low); }
.review-placeholder { display: grid; justify-items: start; gap: var(--space-2); padding: var(--space-6); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.review-placeholder p { margin: 0; }
.review-summary { display: grid; gap: var(--space-4); padding: var(--space-4) var(--space-5); }
.summary-head { display: flex; align-items: flex-start; justify-content: space-between; gap: var(--space-3); }
.summary-head h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-large); }
.summary-meta { margin: var(--space-1) 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.readiness { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: var(--space-2); margin: 0; padding: 0; list-style: none; }
.readiness li { display: grid; grid-template-columns: auto 1fr; gap: 0 var(--space-2); align-items: center; padding: var(--space-3) var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-large); }
.readiness li small { grid-column: 2; font: var(--md-sys-body-small); }
.readiness li[data-ready='true'] { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.review-tabs { display: grid; gap: var(--space-3); }
.tab-list { display: flex; flex-wrap: wrap; gap: var(--space-1); border-bottom: 1px solid var(--md-sys-color-outline-variant); }
.tab-trigger { min-height: 44px; padding: 0 var(--space-4); border: 0; border-bottom: 3px solid transparent; background: transparent; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-title-small); cursor: pointer; }
.tab-trigger[data-state='active'] { border-bottom-color: var(--md-sys-color-primary); color: var(--md-sys-color-primary); }
.tab-trigger:focus-visible { outline: 2px solid var(--md-sys-color-primary); outline-offset: -2px; }
.tab-pane { display: grid; gap: var(--space-3); }
.tab-pane[hidden] { display: none; }
.tab-pane:focus-visible { outline: 2px solid var(--md-sys-color-primary); outline-offset: 2px; }
.pane-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--space-3); }
.pane-card { display: grid; align-content: start; gap: var(--space-3); padding: var(--space-4); min-width: 0; }
.pane-card h4 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.pane-note { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.meta-grid { display: grid; grid-template-columns: minmax(96px, 140px) minmax(0, 1fr); gap: var(--space-2) var(--space-4); margin: 0; }
.meta-grid dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.meta-grid dd { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); overflow-wrap: anywhere; }
.row-hint { display: block; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.history-list { display: grid; gap: var(--space-2); margin: 0; padding: 0; list-style: none; font: var(--md-sys-body-medium); }
.history-list li { display: flex; flex-wrap: wrap; gap: var(--space-1) var(--space-3); align-items: baseline; }
.attempt { color: var(--md-sys-color-on-surface-variant); }
.attempt[data-state='failed'] { color: var(--md-sys-color-error); }
.attempt code { margin-left: var(--space-1); font: var(--md-sys-label-medium); }
.step-list { display: grid; gap: var(--space-2); margin: 0; padding: 0; list-style: none; }
.step { display: grid; gap: var(--space-1); padding: var(--space-3) var(--space-3); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); }
.step p { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.step-head { display: flex; align-items: center; gap: var(--space-2); }
.step-points { margin-left: auto; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.role-chip { padding: 2px var(--space-2); border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-container-high); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.step[data-role='score'] .role-chip { background: var(--md-sys-color-primary-container); color: var(--md-sys-color-on-primary-container); }
.step-details { margin: 0; padding-left: var(--space-5); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.file-list { display: grid; gap: var(--space-1); margin: 0; padding: 0; list-style: none; font: var(--md-sys-body-medium); }
.file-list li { display: flex; justify-content: space-between; gap: var(--space-3); }
.file-list span { color: var(--md-sys-color-on-surface-variant); }
.spec-details summary { color: var(--md-sys-color-primary); cursor: pointer; font: var(--md-sys-label-large); }
.spec-details pre { max-height: 320px; overflow: auto; padding: var(--space-3); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container); font: var(--md-sys-body-small); }
.approve-card { display: grid; gap: var(--space-3); padding: var(--space-4) var(--space-5); }
.approve-card h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.confirm-row { display: flex; gap: var(--space-2); align-items: flex-start; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.reason-field { display: grid; gap: var(--space-1); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.reason-field textarea { padding: var(--space-2) var(--space-3); border: 1px solid var(--md-sys-color-outline); border-radius: var(--md-sys-shape-extra-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); resize: vertical; }
.approve-actions { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2) var(--space-4); }
.approved { display: flex; align-items: center; gap: var(--space-2); color: var(--md-sys-color-primary); font: var(--md-sys-title-small); }
.approved span { color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.approved a, .publication a { color: var(--md-sys-color-primary); }
.publication { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2); font: var(--md-sys-body-medium); }
.state-chip { padding: 2px var(--space-3); border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-container-high); font: var(--md-sys-label-medium); }
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
  .meta-grid dd { margin-bottom: var(--space-2); }
}
</style>
