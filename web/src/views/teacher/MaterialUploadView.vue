<template>
  <div class="material-upload">
    <header class="page-header">
      <h2>材料上传与 AgentRun</h2>
      <p class="page-subtitle">上传题面、Starter 和样例，确认 LLM 出站策略后启动 AgentRun。</p>
    </header>

    <DiagnosticBanner
      v-if="isContextMissing"
      code="COURSE_CONTEXT_MISSING"
      message="课程上下文未绑定，无法加载 LLM 出站策略与材料上传。请通过课程选择器选择课程或联系管理员完成 #47。"
      :retryable="false"
      severity="error"
    />
    <DiagnosticBanner
      v-else-if="isContextFromEnv"
      code="COURSE_CONTEXT_FROM_ENV"
      message="当前使用部署配置中的默认课程上下文；真实课程选择待 #47 接入。"
      :retryable="false"
      severity="warning"
    />

    <section class="policy-section" aria-labelledby="policy-heading">
      <h3 id="policy-heading" class="section-title">
        <SvgIcon name="policy" size="sm" aria-hidden="true" />
        课程 LLM 出站策略
      </h3>
      <AsyncStateView v-if="!isContextMissing" :state="policy.state" @retry="policy.load">
        <template #success="{ data }">
          <div class="policy-card md-card">
            <div class="policy-row">
              <span class="policy-label">Provider / Model</span>
              <span class="policy-value">{{ data.binding.model }}</span>
            </div>
            <div class="policy-row">
              <span class="policy-label">Claude Code 版本</span>
              <span class="policy-value">{{ data.binding.claudeCodeVersion }}</span>
            </div>
            <div class="policy-row">
              <span class="policy-label">Worker 镜像摘要</span>
              <code class="policy-value">{{ truncateSha256(data.binding.workerImageSha256) }}</code>
            </div>
            <div class="policy-row">
              <span class="policy-label">运行时配置摘要</span>
              <code class="policy-value">{{ truncateSha256(data.binding.runtimeConfigSha256) }}</code>
            </div>
            <div class="policy-row">
              <span class="policy-label">硬拒绝分类</span>
              <span class="policy-tags">
                <span v-for="cls in data.deniedDataClasses" :key="cls" class="tag tag--deny">{{ cls }}</span>
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
        </template>
      </AsyncStateView>
    </section>

    <section class="upload-section" aria-labelledby="upload-heading">
      <h3 id="upload-heading" class="section-title">
        <SvgIcon name="upload" size="sm" aria-hidden="true" />
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
        />
        <SvgIcon name="folder_open" size="lg" aria-hidden="true" />
        <p>拖拽文件夹到此处，或点击选择材料文件夹</p>
        <button type="button" class="outlined-button" @click="fileInput?.click()">选择文件夹</button>
      </div>

      <div v-if="upload.files.length > 0" class="file-list">
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
          <template #sha256="{ row }">
            <code :title="row.sha256">{{ truncateSha256(row.sha256) }}</code>
          </template>
          <template #status="{ row }">
            <span class="file-status" :class="`file-status--${row.status}`">
              <template v-if="row.status === 'pending'">待上传</template>
              <template v-else-if="row.status === 'uploading'">上传中 {{ row.progress }}%</template>
              <template v-else-if="row.status === 'done'">完成</template>
              <template v-else-if="row.status === 'error'">失败</template>
            </span>
          </template>
          <template #actions="{ row }">
            <button type="button" class="icon-button text-button" aria-label="移除" @click="upload.removeFile(row.path)">
              <SvgIcon name="delete" size="sm" aria-hidden="true" />
            </button>
          </template>
        </DataTable>
      </div>

      <div v-if="upload.state.kind === 'error'" class="upload-error">
        <DiagnosticBanner
          :code="upload.state.diagnostic.code"
          :message="upload.state.diagnostic.message"
          :retryable="upload.state.diagnostic.retryable"
          severity="error"
          @retry="upload.createSession"
        />
      </div>

      <div class="upload-actions">
        <button
          type="button"
          class="filled-button"
          :disabled="!canUpload"
          @click="upload.createSession"
        >
          <template v-if="upload.state.kind === 'hashing'">计算哈希中…</template>
          <template v-else-if="upload.state.kind === 'creating'">创建会话…</template>
          <template v-else-if="upload.state.kind === 'uploading'">上传中…</template>
          <template v-else-if="upload.state.kind === 'completing'">确认归档…</template>
          <template v-else>上传材料包</template>
        </button>
        <button v-if="packageDone" type="button" class="text-button" @click="upload.clear">清除</button>
      </div>

      <div v-if="packageDone && uploadedPackage" class="package-summary">
        <SvgIcon name="check_circle" size="md" aria-hidden="true" />
        <span>材料包已归档：{{ uploadedPackage.id }} (rev-{{ uploadedPackage.revision }})</span>
      </div>
    </section>

    <section v-if="packageDone" class="run-section" aria-labelledby="run-heading">
      <h3 id="run-heading" class="section-title">
        <SvgIcon name="smart_toy" size="sm" aria-hidden="true" />
        启动 AgentRun
      </h3>

      <div class="runtime-field">
        <span class="field-label">目标 Runtime</span>
        <div class="runtime-options">
          <label class="radio-option">
            <input v-model="requestedRuntime" type="radio" value="container" />
            <span>Container</span>
          </label>
          <label class="radio-option">
            <input v-model="requestedRuntime" type="radio" value="virtual_machine" />
            <span>Virtual Machine</span>
          </label>
        </div>
      </div>

      <button
        type="button"
        class="filled-button"
        :disabled="!canStartRun"
        @click="startRun"
      >
        启动 AgentRun
      </button>

      <AsyncStateView v-if="agent.run.kind !== 'idle'" :state="agent.run" @retry="retryCurrentRun">
        <template #success="{ data }">
          <div class="run-card md-card">
            <div class="run-header">
              <span class="run-id">{{ data.id }}</span>
              <span class="run-state" :class="`run-state--${data.state}`">{{ data.state }}</span>
            </div>
            <div class="run-actions">
              <button
                v-if="data.state === 'running'"
                type="button"
                class="text-button"
                @click="agent.cancel"
              >
                取消
              </button>
              <template v-if="data.state === 'failed' || data.state === 'partially_succeeded'">
                <button type="button" class="text-button" @click="agent.retryTrack('environment')">重试环境轨道</button>
                <button type="button" class="text-button" @click="agent.retryTrack('evaluation')">重试评测轨道</button>
              </template>
            </div>
          </div>
        </template>
      </AsyncStateView>

      <div v-if="events.length > 0" class="timeline-section">
        <h4 class="section-subtitle">实时事件</h4>
        <EventTimeline :events="events" aria-label="AgentRun 事件时间线" />
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from 'vue'
import { useCourseContext } from '@/composables/useCourseContext'
import { useActiveCourseLlmPolicy } from '@/composables/useActiveCourseLlmPolicy'
import { useProblemPackageUpload } from '@/composables/useProblemPackageUpload'
import { useAgentRun } from '@/composables/useAgentRun'
import { useCourseEventStream } from '@/composables/useCourseEventStream'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import DataTable from '@/components/common/DataTable.vue'
import EventTimeline from '@/components/common/EventTimeline.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { truncateSha256 } from '@/utils/format'
import type { DataTableColumn } from '@/components/common/DataTable.vue'
import type { UploadFile } from '@/composables/useProblemPackageUpload'

const course = useCourseContext()
const courseId = course.courseId
const isContextMissing = computed(() => course.context.value === null)
const isContextFromEnv = computed(() => course.context.value?.source === 'env')

const policy = useActiveCourseLlmPolicy(courseId)
const policyRevision = computed(() => (policy.state.kind === 'success' ? policy.state.data.revision : undefined))
const upload = useProblemPackageUpload(courseId, policyRevision)
const agent = useAgentRun(courseId)
const { events, connect: connectEvents, disconnect: disconnectEvents } = useCourseEventStream(courseId)

const fileInput = ref<HTMLInputElement | null>(null)
const dragOver = ref(false)
const requestedRuntime = ref<'container' | 'virtual_machine'>('container')

const fileColumns: DataTableColumn<UploadFile>[] = [
  { key: 'path', title: '路径' },
  { key: 'sizeBytes', title: '大小' },
  { key: 'sha256', title: 'SHA-256' },
  { key: 'status', title: '状态' },
  { key: 'actions', title: '操作' },
]

const canUpload = computed(() => {
  const ready = upload.state.kind === 'ready' || upload.state.kind === 'error'
  return ready && upload.files.length > 0 && policyRevision.value !== undefined
})

const packageDone = computed(() => upload.state.kind === 'done')
const uploadedPackage = computed(() => (upload.state.kind === 'done' ? upload.state.package : null))

const canStartRun = computed(() => {
  return packageDone.value && uploadedPackage.value && policy.state.kind === 'success' && !agent.polling
})

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
  if (!pkg || !policyData) return
  await agent.start({
    packageId: pkg.id,
    packageRevision: pkg.revision,
    packageSha256: pkg.manifestSha256,
    policyId: policyData.id,
    policyRevision: policyData.revision,
    requestedRuntime: requestedRuntime.value,
  })
}

async function retryCurrentRun() {
  const current = agent.run.kind === 'success' ? agent.run.data : undefined
  if (current) {
    agent.beginPolling(current.id)
  }
}

watch(() => agent.run.kind, (kind, oldKind) => {
  if (kind === 'success' && oldKind !== 'success') {
    connectEvents()
  } else if (kind !== 'success') {
    disconnectEvents()
  }
})

// Release background work when leaving the page: stop the AgentRun poll
// timer and close the SSE stream so neither outlives the view.
onUnmounted(() => {
  agent.stopPolling()
  disconnectEvents()
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
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
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

.run-state--running { background: var(--md-sys-color-primary-container); color: var(--md-sys-color-on-primary-container); }
.run-state--succeeded { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.run-state--failed { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
.run-state--cancelled { background: var(--md-sys-color-surface-container-highest); color: var(--md-sys-color-on-surface-variant); }
.run-state--partially_succeeded { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }

.run-actions {
  display: flex;
  gap: 8px;
}

.timeline-section {
  margin-top: 20px;
}
</style>
