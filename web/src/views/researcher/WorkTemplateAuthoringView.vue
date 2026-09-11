<template>
  <section class="template-panel" aria-labelledby="template-heading">
    <header class="section-heading">
      <div>
        <h3 id="template-heading">生成 Work 模板</h3>
        <p class="section-note">上传项目材料，生成可重复使用的 Work 模板；审核通过后发布给资源申请使用。</p>
      </div>
    </header>

    <DiagnosticBanner
      v-if="!projectId"
      code="PROJECT_CONTEXT_MISSING"
      message="请先选择一个项目，再生成 Work 模板。"
      :retryable="false"
      severity="error"
    />

    <section class="template-card md-card" aria-labelledby="template-policy-heading">
      <div class="section-heading section-heading--compact">
        <div>
          <h4 id="template-policy-heading">项目 AI 设置（高级详情）</h4>
          <p>这些设置约材料整理和模板生成，通常无需调整。</p>
        </div>
        <button type="button" class="icon-button" aria-label="刷新项目策略" :disabled="policy.state.kind === 'loading'" @click="policy.load">
          <SvgIcon name="refresh" size="sm" aria-hidden="true" />
        </button>
      </div>
      <AsyncStateView v-if="projectId" :state="policy.state" empty-text="当前项目没有已激活的 LLM 策略。请先由管理员配置。" @retry="policy.load">
        <template #success="{ data }">
          <div class="policy-summary">
            <div><span>模型</span><code>{{ data.binding.model }}</code></div>
            <div><span>Claude Code</span><code>{{ data.binding.claudeCodeVersion }}</code></div>
            <div><span>策略</span><code>{{ data.id }} / rev-{{ data.revision }}</code></div>
            <div><span>预算</span><code>{{ data.budget.maxRequests }} requests / {{ data.budget.maxInputTokens }} input tokens</code></div>
          </div>
        </template>
      </AsyncStateView>
    </section>

    <section class="template-card md-card" aria-labelledby="package-heading">
      <div class="section-heading section-heading--compact">
        <div>
          <h4 id="package-heading">上传项目材料</h4>
          <p>选择项目目录，系统会整理为可审核的项目材料包。</p>
        </div>
      </div>

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
          data-testid="work-template-file-input"
          @change="onFileInput"
        />
        <SvgIcon name="folder_open" size="lg" aria-hidden="true" />
        <p>拖拽材料文件夹到此处，或选择一个文件夹</p>
        <button type="button" class="outlined-button" @click="fileInput?.click()">选择材料文件夹</button>
      </div>

      <ul v-if="upload.files.length > 0" class="file-list" aria-label="待上传材料文件">
        <li v-for="file in upload.files" :key="file.path" class="file-row">
          <span class="file-path">{{ file.path }}</span>
          <span class="file-size">{{ upload.formatBytes(file.sizeBytes) }}</span>
          <span class="file-status" :class="`file-status--${file.status}`">
            {{ file.status === 'pending' ? '待上传' : file.status === 'uploading' ? `上传中 ${file.progress}%` : file.status === 'done' ? '完成' : '失败' }}
          </span>
          <button type="button" class="text-button" @click="upload.removeFile(file.path)">移除</button>
        </li>
      </ul>

      <DiagnosticBanner
        v-if="upload.state.kind === 'error'"
        :code="upload.state.diagnostic.code"
        :message="upload.state.diagnostic.message"
        :retryable="upload.state.diagnostic.retryable"
        severity="error"
        @retry="upload.retry"
      />

      <div class="form-actions">
        <button type="button" class="filled-button" :disabled="!canUpload" @click="upload.createSession">
          <template v-if="upload.state.kind === 'hashing'">计算哈希中…</template>
          <template v-else-if="upload.state.kind === 'creating'">创建上传会话…</template>
          <template v-else-if="upload.state.kind === 'uploading'">上传中…</template>
          <template v-else-if="upload.state.kind === 'completing'">确认归档…</template>
          <template v-else>上传材料包</template>
        </button>
        <button v-if="packageDone" type="button" class="text-button" @click="upload.clear">清除材料</button>
      </div>

      <p v-if="uploadedPackage" class="package-summary" role="status">
        <SvgIcon name="check_circle" size="md" aria-hidden="true" />
        <span>材料包已归档：{{ uploadedPackage.id }} · rev-{{ uploadedPackage.revision }}</span>
      </p>
    </section>

    <section v-if="packageDone" class="template-card md-card" aria-labelledby="run-heading">
      <div class="section-heading section-heading--compact">
        <div>
          <h4 id="run-heading">生成可重复使用的 Work 模板</h4>
          <p>材料包就绪后启动生成；完成后检查候选运行环境，再提交审核。</p>
        </div>
        <button v-if="agent.run.kind === 'success'" type="button" class="icon-button" aria-label="刷新 AgentRun" @click="agent.load(agent.run.data.id)">
          <SvgIcon name="refresh" size="sm" aria-hidden="true" />
        </button>
      </div>

      <button type="button" class="filled-button" :disabled="!canStartRun" @click="startRun">
        {{ agent.acting === 'start' ? '提交中…' : '启动 Work AgentRun' }}
      </button>

      <AsyncStateView v-if="agent.run.kind !== 'idle'" :state="agent.run" empty-text="启动后这里会显示 AgentRun 状态。" @retry="reloadRun">
        <template #success="{ data }">
          <details class="technical-details">
            <summary>查看生成任务详情</summary>
          <div class="run-overview">
            <div><span>Run</span><code>{{ data.id }}</code></div>
            <div><span>状态</span><span class="state-chip" :class="`state-chip--${data.state}`">{{ runStateLabel(data.state) }}</span></div>
            <div><span>Revision</span><code>rev-{{ data.revision }}</code></div>
          </div>
          <ul class="track-list">
            <li v-for="track in data.tracks" :key="track.kind" class="track-row">
              <span>{{ track.kind === 'environment' ? 'Work Environment 候选' : track.kind === 'evaluation' ? 'Evaluation 候选' : 'Work 配置' }}</span>
              <code v-if="track.candidateId">{{ track.candidateId }}</code>
              <span v-else class="muted">尚未生成</span>
            </li>
          </ul>
          </details>
          <div class="run-actions">
            <button v-if="data.state === 'requested' || data.state === 'running'" type="button" class="outlined-button danger-button" :disabled="agent.acting !== null" @click="agent.cancel">取消 AgentRun</button>
            <button v-if="data.state === 'failed' || data.state === 'partially_succeeded'" type="button" class="text-button" :disabled="agent.acting !== null" @click="agent.retryTrack('environment')">重试 Environment 轨道</button>
          </div>
        </template>
      </AsyncStateView>

      <DiagnosticBanner
        v-if="agent.outcome"
        :code="agent.outcome.code"
        :message="agent.outcome.message"
        :retryable="agent.outcome.retryable"
        :severity="agent.outcome.code.includes('FAILED') ? 'error' : 'info'"
        @retry="reloadRun"
      />
    </section>

    <section v-if="environmentCandidate.kind !== 'idle'" class="template-card md-card" aria-labelledby="candidate-heading" data-testid="work-template-candidate">
      <div class="section-heading section-heading--compact">
        <div>
          <h4 id="candidate-heading">检查生成结果</h4>
          <p>查看运行环境和构建产物，确认它符合当前 Work 项目后提交审核。</p>
        </div>
        <button v-if="candidateId" type="button" class="icon-button" aria-label="刷新 Environment 候选" :disabled="environmentCandidate.kind === 'loading'" @click="loadCandidate(candidateId)">
          <SvgIcon name="refresh" size="sm" aria-hidden="true" />
        </button>
      </div>

      <AsyncStateView :state="environmentCandidate" empty-text="该 AgentRun 尚未生成 Environment 候选。" :loading-text="'加载 Environment 候选…'" @retry="retryCandidate">
        <template #success="{ data }">
          <div class="candidate-summary">
            <div><span>名称</span><strong>{{ data.candidate.spec.name }}</strong></div>
            <div><span>类别</span><span class="state-chip">{{ data.candidate.spec.class }}</span></div>
            <div><span>运行时</span><code>{{ runtimeKindLabel(data.candidate.spec.runtime.kind) }}</code></div>
            <div><span>构建</span><span>{{ buildStateLabel(data.build?.state) }}</span></div>
            <div><span>运行时 artifact</span><code v-if="data.imageArtifact">{{ artifactIdentity(data.imageArtifact) }}</code><span v-else>未就绪</span></div>
          </div>
          <details class="technical-details">
            <summary>查看版本与审批记录</summary>
            <div class="candidate-technical-summary">
              <div><span>候选 Revision</span><code>rev-{{ data.candidate.revision }}</code></div>
              <div><span>策略 Revision</span><code>rev-{{ data.candidate.policyRevision }}</code></div>
              <div><span>信任 Revision</span><code>rev-{{ data.trustRevision }}</code></div>
            </div>
          </details>

          <details class="candidate-details" open>
            <summary>查看完整 EnvironmentSpec</summary>
            <pre>{{ formatCandidateSpec(data.candidate.spec) }}</pre>
          </details>

          <p v-if="data.build?.diagnosticCode" class="candidate-diagnostic" role="alert">{{ data.build.diagnosticCode }}</p>
          <p v-if="data.candidate.spec.class !== 'work'" class="candidate-diagnostic" role="alert">该候选的 Environment class 不是 work，不能发布为 Work 模板。</p>
          <p v-else-if="!data.imageArtifact" class="candidate-diagnostic" role="status">运行时 artifact 尚未就绪。候选生成完成后刷新此卡片。</p>

          <div v-if="data.approvals.length > 0" class="approval-history">
            <h5>批准记录</h5>
            <ul>
              <li v-for="approval in data.approvals" :key="approval.id">
                <span class="state-chip" :class="`state-chip--${approval.decision}`">{{ approval.decision }}</span>
                <code>{{ approval.id }}</code>
                <span>{{ approval.reason }}</span>
              </li>
            </ul>
          </div>

          <DiagnosticBanner
            v-if="candidateOutcome"
            :code="candidateOutcome.code"
            :message="candidateOutcome.message"
            :retryable="candidateOutcome.retryable"
            :severity="candidateOutcome.code.includes('FAILED') ? 'error' : 'info'"
            @retry="retryCandidate"
          />

          <label class="authorization-field candidate-confirmation">
            <input v-model="candidateReviewAcknowledged" data-testid="work-template-candidate-confirmation" type="checkbox" />
            <span>我已查看完整 EnvironmentSpec、运行时 artifact、构建状态和安全约束，并确认这个候选用于当前 Work 项目。</span>
          </label>

          <form v-if="!approvedCandidate" class="approval-form" data-testid="work-template-candidate-approval-form" @submit.prevent="approveCandidate">
            <label>
              <span>候选批准原因</span>
              <textarea v-model="approvalReason" class="text-input" rows="3" maxlength="500" required placeholder="说明为什么批准这个 Work Environment 候选" />
            </label>
            <button type="submit" class="filled-button" :disabled="!canApproveCandidate || approvingCandidate">
              {{ approvingCandidate ? '提交批准中…' : '批准 Environment 候选' }}
            </button>
          </form>
          <div v-else class="approved-summary" role="status">
            <SvgIcon name="check_circle" size="md" aria-hidden="true" />
            <span>候选已批准：{{ approvedCandidate.id }}</span>
          </div>
        </template>
      </AsyncStateView>
    </section>

    <section v-if="approvedCandidate" class="template-card md-card" aria-labelledby="release-heading" data-testid="work-template-release">
      <div class="section-heading section-heading--compact">
        <div>
          <h4 id="release-heading">发布 Work 模板</h4>
          <p>审核通过后发布模板，之后可以申请 Work 资源。</p>
        </div>
      </div>
      <div v-if="environmentCandidate.kind === 'success'" class="release-summary">
        <div><span>Candidate</span><code>{{ environmentCandidate.data.candidate.id }}</code></div>
        <div><span>Runtime</span><code>{{ environmentCandidate.data.candidate.spec.runtime.kind }}</code></div>
        <div><span>Approval</span><code>{{ approvedCandidate.id }}</code></div>
      </div>
      <DiagnosticBanner
        v-if="releaseOutcome"
        :code="releaseOutcome.code"
        :message="releaseOutcome.message"
        :retryable="releaseOutcome.retryable"
        :severity="releaseOutcome.code.includes('FAILED') ? 'error' : 'info'"
        @retry="retryRelease"
      />
      <button v-if="release.kind !== 'success'" type="button" class="filled-button" data-testid="work-template-release-button" :disabled="!canPublishRelease || publishingRelease" @click="publishRelease">
        {{ publishingRelease ? '提交发布中…' : '发布 Work 模板' }}
      </button>
      <div v-else class="operation-summary" role="status">
        <SvgIcon name="check_circle" size="md" aria-hidden="true" />
        <div>
          <strong>Work 模板发布操作已接受</strong>
          <code>{{ release.data.operationId }}</code>
          <a :href="release.data.statusUrl">查看发布状态</a>
          <RouterLink class="filled-button" data-testid="work-template-resource-link" :to="{ path: '/researcher/resources', query: { projectId: props.projectId ?? undefined } }">申请 Work 资源</RouterLink>
        </div>
      </div>
    </section>
  </section>
</template>

<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from 'vue'
import { RouterLink } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { appendProjectEnvironmentCandidateDecision, createEnvironmentTemplateRelease, getEnvironmentTemplateRelease, getProjectEnvironmentCandidate } from '@/generated/contracts'
import type {
  AgentRunSchema,
  CandidateBuildState,
  CandidateApprovalSchema,
  EnvironmentCandidateViewSchema,
  OperationAccepted,
  ProblemPackageSchema,
} from '@/generated/contracts'
import { useActiveProjectLlmPolicy } from '@/composables/useActiveProjectLlmPolicy'
import { useProjectAgentRun } from '@/composables/useProjectAgentRun'
import { useProjectProblemPackageUpload } from '@/composables/useProjectProblemPackageUpload'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

const props = defineProps<{
  projectId: string | null
  courseId?: string | null
  runId?: string | null
  releaseId?: string | null
}>()
const emit = defineEmits<{
  (event: 'run-created', runId: string): void
  (event: 'release-created', releaseId: string): void
}>()

const projectIdRef = computed(() => props.projectId)
const courseIdRef = computed(() => props.courseId ?? null)
const runIdRef = computed(() => props.runId?.trim() || null)
const releaseIdRef = computed(() => props.releaseId?.trim() || null)
const policy = useActiveProjectLlmPolicy(projectIdRef)
const policyRevision = computed(() => policy.state.kind === 'success' ? policy.state.data.revision : undefined)
const upload = useProjectProblemPackageUpload(projectIdRef, policyRevision, courseIdRef)
const agent = useProjectAgentRun(projectIdRef)

const fileInput = ref<HTMLInputElement | null>(null)
const dragOver = ref(false)
const environmentCandidate = ref<AsyncState<EnvironmentCandidateViewSchema>>({ kind: 'idle' })
const candidateOutcome = ref<DiagnosticViewModel | null>(null)
const releaseOutcome = ref<DiagnosticViewModel | null>(null)
const approvalReason = ref('')
const candidateReviewAcknowledged = ref(false)
const approvingCandidate = ref(false)
const publishingRelease = ref(false)
const approvedCandidate = ref<CandidateApprovalSchema | null>(null)
const release = ref<AsyncState<OperationAccepted>>({ kind: 'idle' })
const releaseRouteState = ref<'none' | 'loading' | 'loaded' | 'error'>('none')
let candidateGeneration = 0
let candidateKey = ''
let candidatePollTimer: ReturnType<typeof setTimeout> | null = null
const CANDIDATE_POLL_INTERVAL_MS = 3000
const CANDIDATE_NOT_FOUND_MAX_RETRIES = 100
let candidateNotFoundRetryId: string | null = null
let candidateNotFoundRetryCount = 0
let loadedRouteRunId: string | null = null
let releaseGeneration = 0
let approvalRequestKey: string | null = null
let releaseRequestKey: string | null = null

const packageDone = computed(() => upload.state.kind === 'done')
const uploadedPackage = computed<ProblemPackageSchema | null>(() => upload.state.kind === 'done' ? upload.state.package : null)
const candidateId = computed(() => {
  if (agent.run.kind !== 'success') return null
  return agent.run.data.tracks.find((track) => track.kind === 'environment')?.candidateId ?? null
})
const canUpload = computed(() => {
  const ready = upload.state.kind === 'ready' || upload.state.kind === 'error'
  return Boolean(props.projectId && ready && upload.files.length > 0 && policyRevision.value !== undefined)
})
const canStartRun = computed(() => Boolean(packageDone.value && uploadedPackage.value && policy.state.kind === 'success' && !agent.acting))
const candidateArtifactReady = computed(() => {
  if (environmentCandidate.value.kind !== 'success') return false
  return candidateArtifactIsReady(environmentCandidate.value.data)
})
const canApproveCandidate = computed(() => {
  if (environmentCandidate.value.kind !== 'success' || approvingCandidate.value || approvedCandidate.value) return false
  return candidateArtifactReady.value && candidateReviewAcknowledged.value && approvalReason.value.trim().length > 0
})
const canPublishRelease = computed(() => {
  if (environmentCandidate.value.kind !== 'success' || !approvedCandidate.value || publishingRelease.value || release.value.kind === 'success') return false
  if (releaseIdRef.value && releaseRouteState.value !== 'loaded') return false
  return candidateArtifactReady.value && candidateReviewAcknowledged.value
})

watch(
  () => agent.run,
  (state) => {
    if (state.kind !== 'success') return
    const nextCandidateId = state.data.tracks.find((track) => track.kind === 'environment')?.candidateId ?? null
    const nextKey = `${state.data.id}:${nextCandidateId ?? ''}`
    if (nextKey === candidateKey) return
    candidateKey = nextKey
    if (!nextCandidateId) {
      environmentCandidate.value = state.data.state === 'failed' || state.data.state === 'cancelled'
        ? { kind: 'blocked', diagnostic: makeDiagnostic('WORK_TEMPLATE_CANDIDATE_MISSING', 'AgentRun 未生成可用的 Work Environment 候选。', false) }
        : { kind: 'loading', message: '等待 Agent 生成 Work Environment 候选…' }
      stopCandidatePolling()
      return
    }
    void loadCandidate(nextCandidateId)
  },
)

watch(
  [projectIdRef, runIdRef],
  ([projectId, runId]) => {
    if (!projectId || !runId) {
      loadedRouteRunId = null
      return
    }
    if (runId === loadedRouteRunId) return
    candidateGeneration += 1
    candidateKey = ''
    environmentCandidate.value = { kind: 'idle' }
    candidateOutcome.value = null
    releaseOutcome.value = null
    approvedCandidate.value = null
    candidateReviewAcknowledged.value = false
    release.value = { kind: 'idle' }
    releaseRouteState.value = releaseIdRef.value ? 'loading' : 'none'
    stopCandidatePolling()
    loadedRouteRunId = runId
    void agent.load(runId)
  },
  { immediate: true },
)

watch(
  [projectIdRef, releaseIdRef],
  ([projectId, releaseId]) => {
    releaseGeneration += 1
    if (!projectId || !releaseId) {
      releaseRouteState.value = 'none'
      if (!releaseId) release.value = { kind: 'idle' }
      return
    }
    void loadRelease(releaseId)
  },
  { immediate: true },
)

watch(
  () => props.projectId,
  () => {
    candidateGeneration += 1
    releaseGeneration += 1
    candidateKey = ''
    environmentCandidate.value = { kind: 'idle' }
    candidateOutcome.value = null
    releaseOutcome.value = null
    approvedCandidate.value = null
    candidateReviewAcknowledged.value = false
    release.value = { kind: 'idle' }
    releaseRouteState.value = releaseIdRef.value ? 'loading' : 'none'
    approvalReason.value = ''
    approvalRequestKey = null
    releaseRequestKey = null
    stopCandidatePolling()
  },
)

async function loadCandidate(id: string, silent = false) {
  const projectId = props.projectId
  const generation = ++candidateGeneration
  stopCandidatePolling()
  if (!silent || candidateNotFoundRetryId !== id) {
    candidateNotFoundRetryId = id
    candidateNotFoundRetryCount = 0
  }
  if (!projectId || !id) {
    environmentCandidate.value = { kind: 'blocked', diagnostic: makeDiagnostic('WORK_TEMPLATE_CANDIDATE_CONTEXT_MISSING', '缺少项目或候选 ID。', false) }
    return
  }
  if (!silent) environmentCandidate.value = { kind: 'loading', message: '加载 Environment 候选…' }
  const result = await getProjectEnvironmentCandidate({ path: { projectId, candidateId: id } })
  if (generation !== candidateGeneration || props.projectId !== projectId) return
  if (result.error) {
    const problem = extractProblemDetails(result.error)
    const candidateProjectionPending = result.response?.status === 404 && problem?.diagnosticCode === 'LW_CANDIDATE_NOT_FOUND'
    if (candidateProjectionPending && candidateNotFoundRetryCount < CANDIDATE_NOT_FOUND_MAX_RETRIES) {
      candidateNotFoundRetryCount += 1
      environmentCandidate.value = { kind: 'loading', message: '等待 Environment 候选同步…' }
      scheduleCandidateNotFoundRetry(projectId, id, generation)
      return
    }
    if (candidateProjectionPending) {
      environmentCandidate.value = { kind: 'error', diagnostic: makeDiagnostic('LW_CANDIDATE_NOT_FOUND', 'Environment 候选在限定时间内仍未同步，请重试。', true) }
      return
    }
    environmentCandidate.value = { kind: 'error', diagnostic: makeDiagnostic(problem?.diagnosticCode ?? 'WORK_TEMPLATE_CANDIDATE_LOAD_FAILED', problem?.detail ?? '加载 Work Environment 候选失败', problem?.retryable ?? true) }
    return
  }
  candidateNotFoundRetryCount = 0
  environmentCandidate.value = { kind: 'success', data: result.data }
  const existing = result.data.approvals.slice().reverse().find((approval) => approval.decision === 'approved' && approval.candidateRevision === result.data.candidate.revision && approval.policyRevision === result.data.candidate.policyRevision && approval.trustRevision === result.data.trustRevision)
  approvedCandidate.value = existing ?? null
  candidateOutcome.value = null
  if (!releaseIdRef.value) {
    releaseOutcome.value = null
    release.value = { kind: 'idle' }
    releaseRouteState.value = 'none'
  }
  approvalReason.value = ''
  candidateReviewAcknowledged.value = false
  approvalRequestKey = null
  releaseRequestKey = null
  scheduleCandidatePoll(result.data)
}

function candidateArtifactIsReady(data: EnvironmentCandidateViewSchema): boolean {
  if (data.candidate.spec.class !== 'work' || data.imageArtifact == null) return false
  if (data.candidate.spec.runtime.kind === 'virtual_machine') return data.build == null || data.build.state === 'succeeded'
  return data.build?.state === 'succeeded'
}

function scheduleCandidatePoll(data: EnvironmentCandidateViewSchema) {
  stopCandidatePolling()
  if (candidateArtifactIsReady(data) || data.candidate.spec.class !== 'work') return
  if (data.build?.state === 'failed' || data.build?.state === 'cancelled') return
  if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return
  candidatePollTimer = setTimeout(() => void loadCandidate(data.candidate.id, true), CANDIDATE_POLL_INTERVAL_MS)
}

function scheduleCandidateNotFoundRetry(projectId: string, id: string, generation: number) {
  stopCandidatePolling()
  candidatePollTimer = setTimeout(() => {
    candidatePollTimer = null
    if (generation !== candidateGeneration || props.projectId !== projectId) return
    void loadCandidate(id, true)
  }, CANDIDATE_POLL_INTERVAL_MS)
}

function stopCandidatePolling() {
  if (candidatePollTimer) {
    clearTimeout(candidatePollTimer)
    candidatePollTimer = null
  }
}

function onVisibilityChange() {
  if (typeof document === 'undefined') return
  if (document.visibilityState === 'hidden') {
    stopCandidatePolling()
    return
  }
  if (environmentCandidate.value.kind === 'success') scheduleCandidatePoll(environmentCandidate.value.data)
  else if (environmentCandidate.value.kind === 'loading' && props.projectId && candidateNotFoundRetryId && candidateNotFoundRetryCount > 0) {
    scheduleCandidateNotFoundRetry(props.projectId, candidateNotFoundRetryId, candidateGeneration)
  }
}

function retryCandidate() {
  if (candidateId.value) void loadCandidate(candidateId.value)
}

async function loadRelease(id: string, silent = false) {
  const projectId = props.projectId
  const generation = ++releaseGeneration
  if (!projectId || !id) {
    releaseRouteState.value = 'error'
    release.value = { kind: 'blocked', diagnostic: makeDiagnostic('WORK_TEMPLATE_RELEASE_CONTEXT_MISSING', '缺少项目或发布 ID。', false) }
    return
  }
  releaseRouteState.value = 'loading'
  if (!silent) release.value = { kind: 'loading', message: '加载发布状态…' }
  const result = await getEnvironmentTemplateRelease({ path: { projectId, releaseId: id } })
  if (generation !== releaseGeneration || props.projectId !== projectId) return
  if (result.error) {
    const problem = extractProblemDetails(result.error)
    const diagnostic = makeDiagnostic(problem?.diagnosticCode ?? 'WORK_TEMPLATE_RELEASE_LOAD_FAILED', problem?.detail ?? '加载 Work 模板发布状态失败', problem?.retryable ?? true)
    releaseRouteState.value = 'error'
    release.value = { kind: 'error', diagnostic }
    releaseOutcome.value = diagnostic
    return
  }
  releaseRouteState.value = 'loaded'
  release.value = {
    kind: 'success',
    data: {
      operationId: result.data.id,
      revision: result.data.version,
      statusUrl: `/api/v1/projects/${encodeURIComponent(projectId)}/environment-template-releases/${encodeURIComponent(result.data.id)}`,
    },
  }
  releaseOutcome.value = makeDiagnostic('WORK_TEMPLATE_RELEASE_RESTORED', '已从记录恢复 Work 模板发布状态。', false)
}

function retryRelease() {
  if (releaseIdRef.value) void loadRelease(releaseIdRef.value)
  else void publishRelease()
}

async function startRun() {
  const pkg = uploadedPackage.value
  const policyData = policy.state.kind === 'success' ? policy.state.data : null
  const projectId = props.projectId
  if (!projectId || !pkg || !policyData || agent.acting) return
  const started = await agent.start({
    ...(props.courseId ? { courseId: props.courseId } : {}),
    packageId: pkg.id,
    packageRevision: pkg.revision,
    policyId: policyData.id,
    policyRevision: policyData.revision,
    environmentClass: 'work',
  })
  if (started && agent.run.kind === 'success') {
    loadedRouteRunId = agent.run.data.id
    emit('run-created', agent.run.data.id)
  }
}

function reloadRun() {
  if (agent.run.kind === 'success') void agent.load(agent.run.data.id)
}

async function approveCandidate() {
  if (!canApproveCandidate.value || environmentCandidate.value.kind !== 'success' || !props.projectId) return
  const data = environmentCandidate.value.data
  const reason = approvalReason.value.trim()
  if (!approvalRequestKey) approvalRequestKey = idempotencyKey()
  approvingCandidate.value = true
  candidateOutcome.value = null
  try {
    const result = await appendProjectEnvironmentCandidateDecision({
      path: { projectId: props.projectId, candidateId: data.candidate.id },
      headers: { 'Idempotency-Key': approvalRequestKey, 'If-Match': ifMatch(data.candidate.revision) },
      body: {
        candidateRevision: data.candidate.revision,
        policyRevision: data.candidate.policyRevision,
        trustRevision: data.trustRevision,
        decision: 'approved',
        reason,
      },
    })
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      candidateOutcome.value = makeDiagnostic(problem?.diagnosticCode ?? 'WORK_TEMPLATE_CANDIDATE_APPROVAL_FAILED', problem?.detail ?? '批准 Environment 候选失败', problem?.retryable ?? true)
      return
    }
    approvedCandidate.value = result.data
    candidateOutcome.value = makeDiagnostic('WORK_TEMPLATE_CANDIDATE_APPROVED', 'Environment 候选已批准，可以发布 Work 模板。', false)
  } finally {
    approvingCandidate.value = false
  }
}

async function publishRelease() {
  if (!canPublishRelease.value || environmentCandidate.value.kind !== 'success' || !approvedCandidate.value || !props.projectId) return
  const data = environmentCandidate.value.data
  if (!releaseRequestKey) releaseRequestKey = idempotencyKey()
  publishingRelease.value = true
  releaseOutcome.value = null
  release.value = { kind: 'loading', message: '提交 Work 模板发布…' }
  try {
    const result = await createEnvironmentTemplateRelease({
      path: { projectId: props.projectId },
      headers: { 'Idempotency-Key': releaseRequestKey },
      body: {
        projectId: props.projectId,
        ...(props.courseId ? { courseId: props.courseId } : {}),
        candidateId: data.candidate.id,
        candidateRevision: data.candidate.revision,
        runtimeKind: data.candidate.spec.runtime.kind,
        approvalId: approvedCandidate.value.id,
      },
    })
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      const diagnostic = makeDiagnostic(problem?.diagnosticCode ?? 'WORK_TEMPLATE_RELEASE_FAILED', problem?.detail ?? '发布 Work 模板失败', problem?.retryable ?? true)
      release.value = { kind: 'error', diagnostic }
      releaseOutcome.value = diagnostic
      return
    }
    release.value = { kind: 'success', data: result.data }
    releaseRouteState.value = 'loaded'
    const statusSegments = result.data.statusUrl.split('/').filter(Boolean)
    const releaseId = statusSegments[statusSegments.length - 1]
    if (releaseId) emit('release-created', decodeURIComponent(releaseId))
    releaseOutcome.value = makeDiagnostic('WORK_TEMPLATE_RELEASE_ACCEPTED', 'Work 模板发布操作已接受。', false)
  } finally {
    publishingRelease.value = false
  }
}

function onFileInput(event: Event) {
  const target = event.target as HTMLInputElement
  if (target.files && target.files.length > 0) void upload.addFiles(Array.from(target.files))
  target.value = ''
}

function onDrop(event: DragEvent) {
  dragOver.value = false
  if (event.dataTransfer) void upload.addDirectoryItems(event.dataTransfer.items)
}

function runStateLabel(state: AgentRunSchema['state']) {
  return ({ requested: '已提交', running: '运行中', partially_succeeded: '部分成功', succeeded: '已完成', awaiting_approval: '等待批准', failed: '失败', cancelling: '取消中', cancelled: '已取消' } as Record<AgentRunSchema['state'], string>)[state]
}

function runtimeKindLabel(kind: EnvironmentCandidateViewSchema['candidate']['spec']['runtime']['kind']) {
  return kind === 'container' ? '容器' : '虚拟机'
}

function buildStateLabel(state: CandidateBuildState | null | undefined) {
  if (!state) return '未提供'
  return ({ requested: '构建中', succeeded: '构建完成', failed: '构建失败', cancelled: '构建已取消' } as Record<CandidateBuildState, string>)[state] ?? state
}

function formatCandidateSpec(spec: EnvironmentCandidateViewSchema['candidate']['spec']) {
  return JSON.stringify(spec, null, 2)
}

function artifactIdentity(artifact: NonNullable<EnvironmentCandidateViewSchema['imageArtifact']>) {
  if (artifact.kind === 'container') return `${artifact.repository}@${artifact.digest}`
  return `${artifact.base_disk.binding}@${artifact.base_disk.sourceRegistryDigest} (${artifact.format})`
}

if (typeof document !== 'undefined') document.addEventListener('visibilitychange', onVisibilityChange)
onUnmounted(() => {
  candidateGeneration += 1
  stopCandidatePolling()
  agent.stopPolling()
  if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', onVisibilityChange)
})
</script>

<style scoped>
.template-panel { display: grid; gap: 20px; }
.section-heading { display: flex; justify-content: space-between; align-items: flex-start; gap: 16px; }
.section-heading--compact { align-items: center; }
.section-heading h3, .section-heading h4 { margin: 0; color: var(--md-sys-color-on-surface); }
.section-heading h3 { font: var(--md-sys-title-large); }
.section-heading h4 { font: var(--md-sys-title-medium); }
.section-heading p, .section-note { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.eyebrow { margin: 0 0 4px; color: var(--md-sys-color-primary); font: var(--md-sys-label-medium); letter-spacing: .05em; text-transform: uppercase; }
.template-card { display: grid; gap: 16px; padding: 20px; }
.policy-summary, .run-overview, .candidate-summary, .release-summary { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px 14px; padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); font: var(--md-sys-body-small); }
.policy-summary span, .run-overview span, .candidate-summary > div > span:first-child, .release-summary > div > span:first-child { color: var(--md-sys-color-on-surface-variant); }
.policy-summary code, .run-overview code, .candidate-summary code, .release-summary code { color: var(--md-sys-color-on-surface); overflow-wrap: anywhere; }
.drop-zone { display: grid; justify-items: center; gap: 12px; padding: 30px 20px; border: 2px dashed var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-large); background: var(--md-sys-color-surface-container-low); text-align: center; }
.drop-zone--active { border-color: var(--md-sys-color-primary); background: var(--md-sys-color-primary-container); }
.drop-zone p { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.file-input { display: none; }
.file-list, .track-list, .approval-history ul { display: grid; gap: 7px; margin: 0; padding: 0; list-style: none; }
.file-row, .track-row { display: grid; grid-template-columns: minmax(0, 1fr) auto auto auto; align-items: center; gap: 10px; padding: 10px 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); font: var(--md-sys-body-small); }
.file-path, .track-row code { min-width: 0; overflow-wrap: anywhere; color: var(--md-sys-color-on-surface); }
.file-size, .muted { color: var(--md-sys-color-on-surface-variant); }
.file-status--uploading { color: var(--md-sys-color-primary); }
.file-status--done { color: var(--md-sys-color-tertiary); }
.file-status--error { color: var(--md-sys-color-error); }
.form-actions, .run-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }
.filled-button, .outlined-button, .text-button { display: inline-flex; justify-content: center; align-items: center; gap: 8px; min-height: 40px; padding: 0 17px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-large); cursor: pointer; text-decoration: none; }
.filled-button { border: 1px solid var(--md-sys-color-primary); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); }
.outlined-button { border: 1px solid var(--md-sys-color-outline); background: transparent; color: var(--md-sys-color-primary); }
.text-button { min-height: 32px; border: 0; background: transparent; color: var(--md-sys-color-primary); }
.danger-button { color: var(--md-sys-color-error); border-color: var(--md-sys-color-error); }
.filled-button:disabled, .outlined-button:disabled, .text-button:disabled { opacity: .5; cursor: not-allowed; }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
.icon-button:disabled { opacity: .5; cursor: not-allowed; }
.state-chip { display: inline-flex; width: fit-content; padding: 3px 8px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.state-chip--approved, .state-chip--succeeded { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--failed, .state-chip--rejected, .state-chip--cancelled { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
.text-input { box-sizing: border-box; width: 100%; min-height: 40px; padding: 9px 12px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.approval-form { display: grid; gap: 12px; }
.approval-form label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.approval-history { display: grid; gap: 8px; }
.approval-history h5 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.approval-history li { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; padding: 8px 0; border-bottom: 1px solid var(--md-sys-color-outline-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.candidate-details { border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); padding: 10px 12px; color: var(--md-sys-color-on-surface-variant); }
.candidate-details summary { cursor: pointer; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); }
.candidate-details pre { max-height: 320px; margin: 10px 0 0; overflow: auto; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-small); white-space: pre-wrap; overflow-wrap: anywhere; }
.technical-details { display: grid; gap: 12px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); padding: 10px 12px; }
.technical-details summary { cursor: pointer; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); }
.candidate-technical-summary { display: grid; gap: 8px; }
.candidate-confirmation { align-items: flex-start; }
.candidate-confirmation input { flex: 0 0 auto; margin-top: 3px; }
.candidate-diagnostic { margin: 0; padding: 10px 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); font: var(--md-sys-body-medium); }
.approved-summary, .package-summary, .operation-summary { display: flex; align-items: center; gap: 10px; padding: 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); font: var(--md-sys-body-medium); }
.operation-summary div { display: grid; gap: 4px; }
.operation-summary code { overflow-wrap: anywhere; }
.operation-summary a { color: inherit; }
@media (max-width: 680px) { .file-row, .track-row { grid-template-columns: 1fr auto; } .file-size, .file-status { justify-self: start; } .section-heading { flex-direction: column; } .section-heading--compact { flex-direction: row; } }
</style>
