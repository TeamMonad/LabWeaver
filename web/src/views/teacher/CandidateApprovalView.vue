<template>
  <div class="candidate-approval">
    <header class="page-header">
      <div>
        <h2>实验包批准</h2>
        <p class="page-subtitle">检查同一 Project 下的材料包、Environment 候选、Evaluation 候选和镜像身份，一次批准完整实验包。</p>
      </div>
      <div class="review-context md-card">
        <label class="context-field">
          <span>Project</span>
          <select v-model="selectedProjectId" class="text-input" aria-label="选择项目" :disabled="projects.projects.kind !== 'success'">
            <option v-for="project in projectOptions" :key="project.id" :value="project.id">{{ project.name }} · {{ project.id }}</option>
          </select>
        </label>
        <label class="context-field">
          <span>AgentRun ID</span>
          <input v-model="runIdInput" class="text-input run-id-input" type="text" placeholder="粘贴 AgentRun ID" aria-label="AgentRun ID" />
        </label>
        <button type="button" class="outlined-button" :disabled="!selectedProjectId || !runIdInput.trim()" @click="authoring.load">加载审核材料</button>
      </div>
    </header>

    <AsyncStateView :state="projects.projects" empty-text="没有可访问的项目，请先创建项目。" @retry="projects.load">
      <template #success>
        <div v-if="projectOptions.length === 0" class="state-note">没有可访问的项目。</div>
      </template>
    </AsyncStateView>

    <AsyncStateView :state="authoring.run" @retry="authoring.load">
      <template #success="{ data: run }">
        <section class="run-summary md-card" aria-labelledby="run-heading">
          <div class="section-heading">
            <h3 id="run-heading">AgentRun</h3>
            <GcpStatusPill :state="run.state" domain="agent" />
          </div>
          <dl class="meta-grid">
            <dt>Run ID</dt>
            <dd><code>{{ run.id }}</code></dd>
            <dt>Project</dt>
            <dd><code>{{ run.projectId }}</code></dd>
            <dt>材料包</dt>
            <dd><code>{{ run.packageId }}</code> · rev-{{ run.revision }}</dd>
            <dt>目标 Runtime</dt>
            <dd>{{ environmentRuntimeLabel }}</dd>
          </dl>
          <p v-if="run.state === 'failed' || run.state === 'cancelled'" class="state-note">AgentRun 未成功完成，无法批准。</p>
        </section>

        <div class="candidate-grid">
          <section class="candidate-section" aria-labelledby="environment-heading">
            <h3 id="environment-heading" class="section-title">Environment 候选</h3>
            <AsyncStateView :state="authoring.environmentCandidate" @retry="authoring.load">
              <template #success="{ data: view }">
                <article class="candidate-card md-card">
                  <dl class="meta-grid">
                    <dt>候选 ID</dt>
                    <dd><code>{{ view.candidate.id }}</code></dd>
                    <dt>Revision</dt>
                    <dd>rev-{{ view.candidate.revision }}</dd>
                    <dt>环境类</dt>
                    <dd>{{ view.candidate.spec.class === 'work' ? 'Work' : 'Experiment' }}</dd>
                    <dt>名称</dt>
                    <dd>{{ view.candidate.spec.name }}</dd>
                    <dt>候选审批记录</dt>
                    <dd>{{ view.approvals.length }} 条（只读）</dd>
                  </dl>
                  <details class="spec-details">
                    <summary>查看 Environment 配置</summary>
                    <pre>{{ formatJson(view.candidate.spec) }}</pre>
                  </details>
                  <div v-if="view.build?.artifact" class="artifact-summary">
                    <h4>运行时镜像身份</h4>
                    <dl class="meta-grid">
                      <template v-for="entry in artifactEntries(view.build.artifact)" :key="entry.label">
                        <dt>{{ entry.label }}</dt>
                        <dd><code>{{ entry.value }}</code></dd>
                      </template>
                    </dl>
                  </div>
                  <DiagnosticBanner
                    v-if="view.build?.diagnosticCode"
                    :code="view.build.diagnosticCode"
                    :message="`构建状态：${view.build.state}`"
                    :retryable="false"
                    severity="warning"
                  />
                </article>
              </template>
            </AsyncStateView>
          </section>

          <section class="candidate-section" aria-labelledby="evaluation-heading">
            <h3 id="evaluation-heading" class="section-title">Evaluation 候选</h3>
            <AsyncStateView :state="authoring.evaluationCandidate" @retry="authoring.load">
              <template #success="{ data: view }">
                <article class="candidate-card md-card">
                  <dl class="meta-grid">
                    <dt>候选 ID</dt>
                    <dd><code>{{ view.candidate.id }}</code></dd>
                    <dt>Revision</dt>
                    <dd>rev-{{ view.candidate.revision }}</dd>
                    <dt>名称</dt>
                    <dd>{{ view.candidate.spec.metadata.name }} · {{ view.candidate.spec.metadata.version }}</dd>
                    <dt>步骤</dt>
                    <dd>{{ view.candidate.spec.spec.steps.length }} 个</dd>
                    <dt>候选审批记录</dt>
                    <dd>{{ view.approvals.length }} 条（只读）</dd>
                  </dl>
                  <details class="spec-details">
                    <summary>查看 Evaluation 配置</summary>
                    <pre>{{ formatJson(view.candidate.spec) }}</pre>
                  </details>
                </article>
              </template>
            </AsyncStateView>
          </section>
        </div>

        <section class="package-section" aria-labelledby="package-heading">
          <h3 id="package-heading" class="section-title">材料包与批准</h3>
          <AsyncStateView :state="authoring.problemPackage" @retry="authoring.load">
            <template #success="{ data: pkg }">
              <article class="approval-card md-card">
                <dl class="meta-grid">
                  <dt>Package ID</dt>
                  <dd><code>{{ pkg.id }}</code></dd>
                  <dt>Package Revision</dt>
                  <dd>rev-{{ pkg.revision }}</dd>
                  <dt>文件数量</dt>
                  <dd>{{ pkg.files.length }}</dd>
                  <dt>完成时间</dt>
                  <dd>{{ formatTimestamp(pkg.completedAt) }}</dd>
                </dl>

                <div v-if="authoring.imageArtifact" class="bound-artifact">
                  <h4>将绑定的完整镜像身份</h4>
                  <dl class="meta-grid">
                    <template v-for="entry in artifactEntries(authoring.imageArtifact)" :key="entry.label">
                      <dt>{{ entry.label }}</dt>
                      <dd><code>{{ entry.value }}</code></dd>
                    </template>
                  </dl>
                </div>

                <DiagnosticBanner
                  v-if="authoring.approval.kind === 'error'"
                  :code="authoring.approval.diagnostic.code"
                  :message="authoring.approval.diagnostic.message"
                  :retryable="authoring.approval.diagnostic.retryable"
                  severity="error"
                  @retry="authoring.load"
                />
                <div v-if="authoring.approval.kind === 'success'" class="approval-success" role="status">
                  <SvgIcon name="check_circle" size="md" aria-hidden="true" />
                  <span>完整实验包已批准：<code>{{ authoring.approval.data.id }}</code>（rev-{{ authoring.approval.data.revision }}）</span>
                </div>

                <div v-if="authoring.approval.kind !== 'success'" class="approval-form">
                  <label class="confirmation-row">
                    <input v-model="ownerConfirmed" type="checkbox" />
                    <span>我已确认材料包、两个候选和镜像身份属于该 Project，并完成了发布前审阅。</span>
                  </label>
                  <label class="reason-field">
                    <span>批准理由（必填，1-500 字）</span>
                    <textarea v-model="reason" class="reason-input" rows="3" maxlength="500" placeholder="记录本次完整实验包批准的依据" />
                  </label>
                  <button type="button" class="filled-button" :disabled="!canSubmit" @click="completeApproval">批准完整实验包</button>
                  <p v-if="!authoring.canApprove" class="state-note">需要同时加载两个候选、材料包和已解析的运行时镜像身份。</p>
                </div>
              </article>
            </template>
          </AsyncStateView>
        </section>
      </template>
    </AsyncStateView>

    <section v-if="authoring.publication.kind !== 'idle'" class="publication-section" aria-labelledby="publication-heading">
      <h3 id="publication-heading" class="section-title">发布状态</h3>
      <AsyncStateView :state="authoring.publication" @retry="authoring.loadPublication">
        <template #success="{ data }">
          <div class="publication-status md-card" role="status" :data-status="data.status">
            <div class="publication-status__header">
              <span>Approval <code>{{ data.approval.id }}</code></span>
              <span class="state-chip" :class="`state-chip--${data.status}`">{{ publicationStateLabel(data.status) }}</span>
            </div>
            <p v-if="data.diagnosticCode" class="publication-status__diagnostic"><code>{{ data.diagnosticCode }}</code></p>
            <p v-if="data.status === 'ready'" class="publication-status__detail">
              Environment Release <code>{{ data.environmentReleaseId }}</code> · Evaluation Release <code>{{ data.evaluationReleaseId }}</code>
            </p>
            <p v-else-if="data.status === 'failed'" class="publication-status__detail">发布未完成，请根据诊断码处理后重新检查。</p>
            <p v-else class="publication-status__detail">下游发布仍在处理，页面会继续读取服务端状态。</p>
          </div>
        </template>
      </AsyncStateView>
    </section>

    <DiagnosticBanner
      v-if="authoring.run.kind === 'blocked' && !runIdInput.trim()"
      code="PROJECT_APPROVAL_RUN_ID_REQUIRED"
      message="请从材料上传页进入，或粘贴要审核的 AgentRun ID。"
      :retryable="false"
      severity="info"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import GcpStatusPill from '@/components/common/GcpStatusPill.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useProjects } from '@/composables/useProjects'
import { useProjectAuthoringApproval } from '@/composables/useProjectAuthoringApproval'
import type { AuthoringPublicationState, CompleteAuthoringApprovalRequestSchemaImageArtifact, ProjectSchema } from '@/generated/contracts'

const route = useRoute()
const router = useRouter()
const projects = useProjects()
const runIdInput = ref(typeof route.query.runId === 'string' ? route.query.runId : '')
const approvalId = ref(typeof route.query.approvalId === 'string' ? route.query.approvalId : undefined)
const reason = ref('')
const ownerConfirmed = ref(false)

const projectOptions = computed<ProjectSchema[]>(() => projects.projects.kind === 'success' ? projects.projects.data : [])
const selectedProjectId = computed({
  get: () => projects.selectedProjectId ?? '',
  set: (id: string) => {
    if (id) projects.select(id)
  },
})
const projectId = computed(() => projects.selectedProjectId)
const runId = computed(() => runIdInput.value.trim() || undefined)
const authoring = useProjectAuthoringApproval(projectId, runId, approvalId)
const environmentRuntimeLabel = computed(() => {
  if (authoring.environmentCandidate.kind !== 'success') return '未知'
  return authoring.environmentCandidate.data.candidate.spec.runtime.kind === 'virtual_machine'
    ? 'Virtual Machine'
    : 'Container'
})

watch(
  () => projects.projects,
  (state) => {
    if (state.kind !== 'success') return
    const requestedProjectId = typeof route.query.projectId === 'string' ? route.query.projectId : undefined
    if (requestedProjectId && state.data.some((project) => project.id === requestedProjectId)) projects.select(requestedProjectId)
  },
  { immediate: true },
)

watch(
  () => authoring.approval.kind === 'success' ? authoring.approval.data.id : undefined,
  (id) => {
    if (!id || approvalId.value === id) return
    approvalId.value = id
    void router.replace({ query: { ...route.query, approvalId: id } })
  },
)

const canSubmit = computed(() => {
  const length = reason.value.trim().length
  return ownerConfirmed.value && length >= 1 && length <= 500 && authoring.canApprove && !authoring.acting
})

async function completeApproval() {
  if (!canSubmit.value) return
  await authoring.complete(reason.value)
}

function formatJson(value: unknown): string {
  return JSON.stringify(value, null, 2)
}

function formatTimestamp(value: string): string {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString()
}

function artifactEntries(artifact: CompleteAuthoringApprovalRequestSchemaImageArtifact) {
  if (artifact.kind === 'container') {
    return [
      { label: '类型', value: 'Container' },
      { label: 'Artifact ID', value: artifact.id },
      { label: 'Build Request', value: artifact.build_request_id },
      { label: 'Repository', value: artifact.repository },
      { label: 'Digest', value: artifact.digest },
    ]
  }
  return [
    { label: '类型', value: 'Virtual Machine' },
    { label: 'Artifact ID', value: artifact.id },
    { label: '格式', value: artifact.format },
    { label: 'Base Disk Binding', value: artifact.base_disk.binding },
    { label: '容量', value: `${artifact.base_disk.capacityBytes} bytes` },
    { label: '源镜像 Digest', value: artifact.base_disk.sourceRegistryDigest },
  ]
}

function publicationStateLabel(state: AuthoringPublicationState): string {
  return ({ pending: '等待发布', publishing: '发布中', ready: '已就绪', failed: '发布失败' } as Record<AuthoringPublicationState, string>)[state]
}
</script>

<style scoped>
.candidate-approval { display: flex; flex-direction: column; gap: 24px; }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 24px; flex-wrap: wrap; }
.page-header h2 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-headline-small); }
.page-subtitle { margin: 4px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.review-context { display: grid; grid-template-columns: minmax(180px, 1fr) minmax(180px, 1fr) auto; gap: 12px; align-items: end; padding: 14px; min-width: min(100%, 720px); }
.context-field, .reason-field { display: flex; flex-direction: column; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.text-input { box-sizing: border-box; min-height: 40px; padding: 0 12px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.run-id-input { min-width: 0; }
.outlined-button, .filled-button { min-height: 40px; padding: 0 18px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-large); cursor: pointer; }
.outlined-button { border: 1px solid var(--md-sys-color-outline); background: transparent; color: var(--md-sys-color-primary); }
.filled-button { border: 0; background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); }
.outlined-button:disabled, .filled-button:disabled { opacity: .5; cursor: not-allowed; }
.md-card { border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); background: var(--md-sys-color-surface-container-low); }
.run-summary, .candidate-card, .approval-card { padding: 16px; }
.section-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.section-heading h3, .section-title { margin: 0 0 12px; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.section-heading h3 { margin: 0; }
.candidate-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 20px; }
.candidate-section, .package-section { min-width: 0; }
.meta-grid { display: grid; grid-template-columns: minmax(120px, 170px) minmax(0, 1fr); gap: 9px 16px; margin: 0; }
.meta-grid dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.meta-grid dd { min-width: 0; margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); overflow-wrap: anywhere; }
.spec-details { margin-top: 16px; }
.spec-details summary { color: var(--md-sys-color-primary); cursor: pointer; font: var(--md-sys-label-large); }
.spec-details pre { max-height: 300px; overflow: auto; margin: 10px 0 0; padding: 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-small); white-space: pre-wrap; }
.artifact-summary, .bound-artifact { margin-top: 18px; padding-top: 14px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.artifact-summary h4, .bound-artifact h4 { margin: 0 0 10px; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.approval-form { display: flex; flex-direction: column; gap: 14px; margin-top: 20px; }
.confirmation-row { display: flex; align-items: flex-start; gap: 10px; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.confirmation-row input { margin-top: 3px; }
.reason-input { box-sizing: border-box; width: 100%; padding: 10px 12px; resize: vertical; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.approval-success { display: flex; align-items: center; gap: 10px; margin-top: 18px; padding: 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-primary-container); color: var(--md-sys-color-on-primary-container); font: var(--md-sys-body-medium); }
.state-note { margin: 12px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.publication-section { display: flex; flex-direction: column; gap: 12px; }
.publication-status { display: grid; gap: 8px; padding: 16px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); background: var(--md-sys-color-surface-container-low); }
.publication-status__header { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.publication-status__detail, .publication-status__diagnostic { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.publication-status__diagnostic { color: var(--md-sys-color-error); }
.state-chip--pending, .state-chip--publishing { background: var(--md-sys-color-primary-container); color: var(--md-sys-color-on-primary-container); }
.state-chip--ready { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.state-chip--failed { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
@media (max-width: 900px) {
  .candidate-grid { grid-template-columns: 1fr; }
  .review-context { grid-template-columns: 1fr; min-width: 0; width: 100%; }
}
</style>
