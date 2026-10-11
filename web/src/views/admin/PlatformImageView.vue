<template>
  <div class="platform-image-page">
    <header class="page-header">
      <div>
        <h2>平台镜像</h2>
        <p class="page-subtitle">维护沙箱可用的容器与虚拟机基础镜像。digest 是权威身份，也是固定且不可变的镜像版本；tag 仅作解析入口。</p>
      </div>
      <button type="button" class="icon-button" aria-label="刷新平台镜像目录" :disabled="busy" @click="images.load">
        <SvgIcon name="refresh" size="sm" aria-hidden="true" />
      </button>
    </header>

    <DiagnosticBanner
      v-if="bannerFailure"
      :code="bannerFailure.code"
      :message="bannerFailure.message"
      :retryable="bannerFailure.retryable"
      severity="error"
      @retry="retryBanner"
    />

    <section class="catalog-card md-card" aria-labelledby="catalog-heading">
      <div class="section-heading">
        <div>
          <h3 id="catalog-heading">目录</h3>
          <p>目录由 Agent 权威持有；引用 release 数是 Control 计算的影响提示。</p>
        </div>
      </div>

      <AsyncStateView :state="catalogState" empty-text="平台镜像目录为空。" @retry="images.load">
        <template #success="{ data }">
          <!-- @vue-generic {CatalogRow} -->
          <DataTable
            :columns="catalogColumns"
            :rows="data"
            class="catalog-table"
            aria-label="平台镜像目录"
          >
            <template #kind="{ row }">{{ kindLabel(row.kind) }}</template>
            <template #sourceReference="{ row }"><code :title="row.sourceReference">{{ row.sourceReference }}</code></template>
            <template #resolvedDigest="{ row }"><code :title="row.resolvedDigest">{{ truncateSha256(row.resolvedDigest) }}</code></template>
            <template #sizeBytes="{ row }">{{ formatBytes(row.sizeBytes) }}</template>
            <template #capacityBytes="{ row }">{{ row.kind === 'virtual_machine' && row.capacityBytes != null ? formatBytes(row.capacityBytes) : '-' }}</template>
            <template #diskSha256="{ row }">{{ row.kind === 'virtual_machine' && row.diskSha256 ? truncateSha256(row.diskSha256) : '-' }}</template>
            <template #format="{ row }">{{ row.kind === 'virtual_machine' && row.format ? row.format : '-' }}</template>
            <template #status="{ row }">
              <span class="state-chip" :class="row.status === 'active' ? 'state-chip--active' : 'state-chip--disabled'">{{ statusLabel(row.status) }}</span>
            </template>
            <template #actions="{ row }">
              <div class="row-actions">
                <button type="button" class="outlined-button small" :disabled="!actionReady || busy" @click="openAction('repin', row)">重新固定</button>
                <button type="button" class="outlined-button small" :disabled="!actionReady || busy" @click="openAction('disable', row)">停用</button>
              </div>
            </template>
          </DataTable>
        </template>
      </AsyncStateView>

      <div class="operation-credentials">
        <label>
          <span>操作原因</span>
          <input v-model="operationReason" class="text-input" maxlength="512" placeholder="记录本次重新固定或停用的原因" />
        </label>
        <label>
          <span>信任版本（重新固定）</span>
          <input v-model.number="repinTrustRevision" class="text-input" type="number" min="1" />
        </label>
        <p class="operation-hint" role="status">
          {{ actionReady ? '操作原因随「重新固定」「停用」提交；信任版本仅用于「重新固定」，digest 始终由服务端权威判定。' : '填写操作原因后可执行「重新固定」或「停用」。' }}
        </p>
      </div>
    </section>

    <section class="register-card md-card" aria-labelledby="register-heading">
      <div class="section-heading">
        <div>
          <h3 id="register-heading">按引用注册</h3>
          <p>只接受配置的单一平台 registry 内的引用；服务端解析 tag 后固定 digest。</p>
        </div>
      </div>
      <form class="admin-form" @submit.prevent="submitRegister">
        <label>
          <span>类型</span>
          <select v-model="registerForm.kind" class="text-input" aria-label="类型">
            <option value="container">container</option>
            <option value="virtual_machine">virtual_machine</option>
          </select>
        </label>
        <label>
          <span>binding</span>
          <input v-model="registerForm.binding" class="text-input" maxlength="128" pattern="[a-z0-9][a-z0-9._-]{0,127}" required />
        </label>
        <label>
          <span>registry 引用（host/repo:tag）</span>
          <input v-model="registerForm.sourceReference" class="text-input" required />
        </label>
        <div class="form-field">
          <label for="register-trust-revision">信任版本</label>
          <input
            id="register-trust-revision"
            v-model.number="registerForm.trustRevision"
            class="text-input"
            type="number"
            min="1"
            required
            aria-describedby="register-trust-revision-hint"
          />
          <small id="register-trust-revision-hint" class="field-hint">用于和平台当前认可的镜像版本匹配；版本不一致时，已发布内容可能无法引用该镜像。</small>
        </div>
        <label class="wide-field">
          <span>原因</span>
          <textarea v-model="registerForm.reason" class="text-input" rows="2" maxlength="512" required />
        </label>
        <button type="submit" class="filled-button" :disabled="busy">注册</button>
      </form>
    </section>

    <section class="upload-card md-card" aria-labelledby="upload-heading">
      <div class="section-heading">
        <div>
          <h3 id="upload-heading">上传归档</h3>
          <p>归档会安全上传到对象存储：容器归档由 Agent 校验每个 blob，虚拟机模板按声明的磁盘路径与容量包装后推送 registry 并登记目录。刷新页面后重新选择同一归档即可继续未完成的上传。</p>
        </div>
      </div>
      <form class="admin-form" @submit.prevent="submitUpload">
        <label>
          <span>类型</span>
          <select v-model="uploadForm.kind" class="text-input" aria-label="类型">
            <option value="container">container</option>
            <option value="virtual_machine">virtual_machine</option>
          </select>
        </label>
        <label>
          <span>binding</span>
          <input v-model="uploadForm.binding" class="text-input" maxlength="128" pattern="[a-z0-9][a-z0-9._-]{0,127}" required />
        </label>
        <label>
          <span>目标引用（host/repo:tag）</span>
          <input v-model="uploadForm.targetReference" class="text-input" required />
        </label>
        <div class="form-field">
          <label for="upload-trust-revision">信任版本</label>
          <input
            id="upload-trust-revision"
            v-model.number="uploadForm.trustRevision"
            class="text-input"
            type="number"
            min="1"
            required
            aria-describedby="upload-trust-revision-hint"
          />
          <small id="upload-trust-revision-hint" class="field-hint">用于和平台当前认可的镜像版本匹配；版本不一致时，已发布内容可能无法引用该镜像。</small>
        </div>
        <template v-if="uploadForm.kind === 'virtual_machine'">
          <div class="form-field">
            <label for="upload-disk-format">磁盘格式</label>
            <select
              id="upload-disk-format"
              v-model="uploadForm.diskFormat"
              class="text-input"
              aria-describedby="upload-disk-format-hint"
            >
              <option value="qcow2">qcow2</option>
              <option value="raw">raw</option>
            </select>
            <small id="upload-disk-format-hint" class="field-hint">必须与归档内磁盘的实际格式一致，否则虚拟机导入或启动可能失败。</small>
          </div>
          <label>
            <span>容量（字节）</span>
            <input v-model="uploadForm.capacityBytes" class="text-input" type="number" min="1" step="1" placeholder="例如 10737418240" required />
          </label>
          <label>
            <span>归档内磁盘路径</span>
            <input v-model="uploadForm.diskPath" class="text-input" maxlength="256" placeholder="disk/disk.img" required />
          </label>
        </template>
        <label class="wide-field">
          <span>原因</span>
          <textarea v-model="uploadForm.reason" class="text-input" rows="2" maxlength="512" required />
        </label>
        <div class="form-field wide-field">
          <label for="upload-archive">{{ uploadForm.kind === 'virtual_machine' ? '虚拟机模板归档（.tar、.tar.gz、.tgz）' : 'OCI 归档（.tar）' }}</label>
          <input
            id="upload-archive"
            ref="fileInput"
            class="text-input"
            type="file"
            :accept="uploadAccept"
            :aria-describedby="uploadForm.kind === 'virtual_machine' ? 'upload-archive-hint' : undefined"
            @change="selectFile"
          />
          <small v-if="uploadForm.kind === 'virtual_machine'" id="upload-archive-hint" class="field-hint">请上传包含单个 qcow2 或 raw 磁盘文件的归档；不能直接上传裸磁盘文件或 OCI 布局。</small>
        </div>
        <p class="upload-limit-hint">归档大小上限：5 GB（5,000,000,000 字节）。</p>
        <button type="submit" class="filled-button" :disabled="busy || !uploadFile">{{ images.uploadNeedsFile ? '继续上传并导入' : '上传并导入' }}</button>
      </form>
      <section v-if="uploadStatusLabel" class="upload-status" role="status" aria-live="polite">
        <div class="upload-status-header">
          <strong>镜像导入：{{ uploadStatusLabel }}</strong>
          <span v-if="uploadProgress !== null">{{ uploadProgress }}%</span>
        </div>
        <p v-if="uploadStatusDiagnostic">{{ uploadStatusDiagnostic.message }}</p>
        <small v-if="uploadStatusDiagnostic" class="upload-diagnostic-code">{{ uploadStatusDiagnostic.code }}</small>
        <div class="row-actions">
          <button
            v-if="images.uploadActive && !images.uploadCancellationPending"
            type="button"
            class="outlined-button small"
            :disabled="cancellingUpload"
            @click="cancelUpload"
          >
            取消上传
          </button>
          <button
            v-if="images.uploadCancellationPending && images.state.kind === 'error'"
            type="button"
            class="outlined-button small"
            :disabled="refreshingUpload"
            @click="refreshUploadStatus"
          >
            {{ refreshingUpload ? '正在刷新…' : '刷新任务状态' }}
          </button>
          <button
            v-if="canRetryUpload"
            type="button"
            class="outlined-button small"
            @click="retryUploadCompletion"
          >
            重试导入
          </button>
        </div>
      </section>
      <DiagnosticBanner
        v-if="uploadDescriptorFailure"
        :code="uploadDescriptorFailure.code"
        :message="uploadDescriptorFailure.message"
        severity="error"
      />
      <p v-if="uploadProgress !== null" class="upload-progress" role="status">上传中 {{ uploadProgress }}%</p>
      <p v-if="uploadFile" class="upload-file">已选择：{{ uploadFile.name }} · {{ formatBytes(uploadFile.size) }}</p>
    </section>

    <ConfirmDialog
      :open="pending !== null"
      :title="confirmTitle"
      :description="confirmDescription"
      :confirm-text="pending?.action === 'repin' ? '重新固定' : '停用'"
      @confirm="confirmAction"
      @cancel="pending = null"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import DataTable, { type DataTableColumn } from '@/components/common/DataTable.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import {
  MAX_PLATFORM_IMAGE_ARCHIVE_BYTES,
  usePlatformImages,
  type UploadPlatformImageInput,
} from '@/composables/usePlatformImages'
import { formatBytes, truncateSha256 } from '@/utils/format'
import { makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import type {
  PlatformImageEntryViewSchema,
  PlatformImageKind,
  PlatformImageStatus,
  PlatformImageUploadState,
  VirtualMachineDiskFormat,
} from '@/generated/contracts'

type CatalogRow = PlatformImageEntryViewSchema & { actions?: never }

type PendingAction = { action: 'repin' | 'disable'; entry: PlatformImageEntryViewSchema }

const images = usePlatformImages()
const fileInput = ref<HTMLInputElement | null>(null)
const uploadFile = ref<File | null>(null)
const operationReason = ref('')
const repinTrustRevision = ref(1)
const pending = ref<PendingAction | null>(null)
const cancellingUpload = ref(false)
const refreshingUpload = ref(false)

const registerForm = reactive<{
  kind: PlatformImageKind
  binding: string
  sourceReference: string
  trustRevision: number
  reason: string
}>({ kind: 'container', binding: '', sourceReference: '', trustRevision: 1, reason: '' })

const uploadForm = reactive<{
  kind: PlatformImageKind
  binding: string
  targetReference: string
  trustRevision: number
  reason: string
  diskFormat: VirtualMachineDiskFormat
  diskPath: string
  capacityBytes: number | string
}>({
  kind: 'container',
  binding: '',
  targetReference: '',
  trustRevision: 1,
  reason: '',
  diskFormat: 'qcow2',
  diskPath: 'disk/disk.img',
  capacityBytes: '',
})

const catalogColumns: DataTableColumn<CatalogRow>[] = [
  { key: 'kind', title: '类型' },
  { key: 'binding', title: 'binding' },
  { key: 'sourceReference', title: '引用' },
  { key: 'resolvedDigest', title: 'digest' },
  { key: 'mediaType', title: '媒体类型' },
  { key: 'sizeBytes', title: '大小' },
  { key: 'capacityBytes', title: '容量' },
  { key: 'diskSha256', title: 'disk_sha256' },
  { key: 'format', title: '格式' },
  { key: 'status', title: '状态' },
  { key: 'trustRevision', title: '信任版本' },
  { key: 'repinGeneration', title: '重固定代次' },
  { key: 'releaseReferenceCount', title: '引用 release 数' },
  { key: 'actions', title: '操作' },
]

const busy = computed(() => (
  images.state.kind === 'loading'
  || images.state.kind === 'uploading'
  || (images.uploadActive && !images.uploadNeedsFile)
))
const actionReady = computed(() => operationReason.value.trim().length > 0)
const uploadProgress = computed(() => (images.state.kind === 'uploading' ? images.state.progress : null))

const UPLOAD_STATE_LABELS: Record<PlatformImageUploadState, string> = {
  pending: '等待启动',
  queued: '排队中',
  freezing: '冻结归档',
  importing: '导入中',
  cancelling: '取消中',
  imported: '已导入',
  failed: '导入失败',
  cancelled: '已取消',
}

const uploadStatusLabel = computed(() => {
  if (images.state.kind === 'uploading') return '上传中'
  if (images.state.kind === 'processing') return UPLOAD_STATE_LABELS[images.state.state]
  if (images.state.kind === 'terminal') return UPLOAD_STATE_LABELS[images.state.state]
  if (images.state.kind === 'error' && images.state.uploadId) return '需要操作'
  return null
})

const uploadStatusDiagnostic = computed(() => {
  if (images.state.kind === 'terminal') return images.state.diagnostic ?? null
  if (images.state.kind === 'error' && images.state.uploadId) return images.state.diagnostic
  return null
})

const canRetryUpload = computed(() => (
  images.state.kind === 'error'
  && Boolean(images.state.uploadId)
  && images.uploadActive
  && images.uploadCompletionRetryable
  && !images.uploadCancellationPending
  && images.state.diagnostic.retryable
))

/** The Agent importer reads VM disks from tar or gzip-compressed tar archives. */
const uploadAccept = computed(() => (uploadForm.kind === 'virtual_machine' ? '.tar,.tar.gz,.tgz' : '.tar'))

/** Client-side descriptor rejection recorded before any upload session is staged. */
const uploadDescriptorFailure = ref<DiagnosticViewModel | null>(null)

/**
 * Mirrors `contracts::valid_vm_disk_upload`'s path rule: a non-empty relative
 * path of at most 256 bytes with no `..`, no leading `/`, no trailing `/`, and
 * no empty segment. The gateway remains authoritative.
 */
function validDiskPath(value: string): string | null {
  const path = value.trim()
  if (!path || path.length > 256 || path.startsWith('/') || path.endsWith('/')) return null
  if (path.includes('..') || path.split('/').some((segment) => segment === '')) return null
  return path
}

/** A capacity is a positive integer byte count; the Agent rejects larger disks. */
function validCapacityBytes(value: number | string): number | null {
  const trimmed = String(value).trim()
  if (!/^[0-9]+$/.test(trimmed)) return null
  const parsed = Number(trimmed)
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : null
}

/**
 * The catalog keeps rendering the last server projection while a mutation is in
 * flight or has failed; only a failed first load replaces the table.
 */
const catalogState = computed<AsyncState<PlatformImageEntryViewSchema[]>>(() => {
  if (images.state.kind === 'error' && images.entries.length === 0) {
    return { kind: 'error', diagnostic: images.state.diagnostic }
  }
  if (images.state.kind === 'idle') return { kind: 'idle' }
  if (images.state.kind === 'loading') return { kind: 'loading', message: '加载平台镜像目录…' }
  return images.entries.length > 0 ? { kind: 'success', data: images.entries } : { kind: 'empty' }
})

const bannerFailure = computed<DiagnosticViewModel | null>(() => {
  if (images.state.kind !== 'error') return null
  return images.entries.length > 0 ? images.state.diagnostic : null
})

const confirmTitle = computed(() => (pending.value?.action === 'repin' ? '重新固定平台镜像' : '停用平台镜像'))

const confirmDescription = computed(() => {
  const current = pending.value
  if (!current) return ''
  if (current.action === 'repin') {
    return '将按已存引用重新解析并替换固定 digest；下游只认 digest，不会自动跟随 tag。'
  }
  return `停用后新创作不再列出该镜像；已有 release 仍按其 digest 运行。当前有 ${current.entry.releaseReferenceCount} 个 release 引用该 digest。`
})

function kindLabel(kind: PlatformImageKind): string {
  return kind === 'container' ? '容器' : '虚拟机'
}

function statusLabel(status: PlatformImageStatus): string {
  return status === 'active' ? '可用' : '已停用'
}

function selectFile(event: Event) {
  const selected = (event.target as HTMLInputElement).files
  uploadDescriptorFailure.value = null
  const file = selected && selected.length > 0 ? selected[0] : null
  if (file && file.size > MAX_PLATFORM_IMAGE_ARCHIVE_BYTES) {
    uploadFile.value = null
    uploadDescriptorFailure.value = makeDiagnostic(
      'PLATFORM_IMAGE_UPLOAD_TOO_LARGE',
      '所选归档超过 5 GB（5,000,000,000 字节）限制。',
      false,
    )
    if (fileInput.value) fileInput.value.value = ''
    return
  }
  uploadFile.value = file
}

function openAction(action: 'repin' | 'disable', entry: PlatformImageEntryViewSchema) {
  pending.value = { action, entry }
}

async function confirmAction() {
  const current = pending.value
  if (!current) return
  pending.value = null
  const reason = operationReason.value.trim()
  if (current.action === 'repin') await images.repin(current.entry, repinTrustRevision.value, reason)
  else await images.disable(current.entry, reason)
}

async function submitRegister() {
  const registered = await images.register({ ...registerForm })
  if (!registered) return
  registerForm.binding = ''
  registerForm.sourceReference = ''
  registerForm.reason = ''
}

async function submitUpload() {
  const file = uploadFile.value
  if (!file) return
  uploadDescriptorFailure.value = null
  const shared = {
    binding: uploadForm.binding,
    targetReference: uploadForm.targetReference,
    trustRevision: uploadForm.trustRevision,
    reason: uploadForm.reason,
  }
  let input: UploadPlatformImageInput = { kind: 'container', ...shared }
  if (uploadForm.kind === 'virtual_machine') {
    const diskPath = validDiskPath(uploadForm.diskPath)
    const capacityBytes = validCapacityBytes(uploadForm.capacityBytes)
    if (diskPath === null || capacityBytes === null) {
      uploadDescriptorFailure.value = makeDiagnostic(
        'PLATFORM_IMAGE_VM_DESCRIPTOR_INVALID',
        '虚拟机模板必须声明 qcow2/raw 格式、正整数容量和归档内的相对磁盘路径，且路径不得包含 `..`。',
      )
      return
    }
    input = {
      kind: 'virtual_machine',
      ...shared,
      diskFormat: uploadForm.diskFormat,
      diskPath,
      capacityBytes,
    }
  }
  const imported = await images.upload(file, input)
  if (!imported) return
  uploadFile.value = null
  if (fileInput.value) fileInput.value.value = ''
  uploadForm.binding = ''
  uploadForm.targetReference = ''
  uploadForm.reason = ''
  uploadForm.diskPath = 'disk/disk.img'
  uploadForm.capacityBytes = ''
}

async function retryBanner() {
  if (
    images.state.kind === 'error'
    && images.state.uploadId
    && images.uploadCompletionRetryable
    && !images.uploadCancellationPending
    && images.state.diagnostic.retryable
  ) {
    await images.retryUploadCompletion()
    return
  }
  if (images.state.kind === 'error' && images.state.uploadId) return
  await images.load()
}

async function retryUploadCompletion() {
  await images.retryUploadCompletion()
}

async function cancelUpload() {
  if (cancellingUpload.value) return
  cancellingUpload.value = true
  try {
    await images.cancelUpload()
  } finally {
    cancellingUpload.value = false
  }
}

async function refreshUploadStatus() {
  if (refreshingUpload.value) return
  refreshingUpload.value = true
  try {
    await images.resumeUpload()
  } finally {
    refreshingUpload.value = false
  }
}

onMounted(async () => {
  await images.load()
  await images.resumeUpload()
})
</script>

<style scoped>
.platform-image-page { display: grid; gap: 20px; }
.page-header, .section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.page-header h2, .section-heading h3 { margin: 0; color: var(--md-sys-color-on-surface); }
.page-header h2 { font: var(--md-sys-headline-small); }
.section-heading h3 { font: var(--md-sys-title-large); }
.page-subtitle, .section-heading p, .operation-hint, .upload-progress, .upload-file, .upload-limit-hint, .upload-status p { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.field-hint { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); line-height: 1.4; }
.catalog-card, .register-card, .upload-card { display: grid; gap: 16px; padding: 20px; }
.admin-form { display: grid; gap: 12px; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); align-items: end; }
.admin-form label, .operation-credentials label, .form-field { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.admin-form .wide-field { grid-column: 1 / -1; }
.admin-form button { justify-self: start; }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
textarea.text-input { resize: vertical; }
.operation-credentials { display: grid; gap: 12px; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); padding-top: 16px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.operation-hint { grid-column: 1 / -1; margin: 0; }
.row-actions { display: flex; gap: 8px; flex-wrap: wrap; }
.upload-status { display: grid; gap: 8px; padding: 12px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-variant); }
.upload-status-header { display: flex; justify-content: space-between; gap: 12px; color: var(--md-sys-color-on-surface); font: var(--md-sys-label-large); }
.state-chip { display: inline-flex; white-space: nowrap; padding: 3px 8px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.state-chip--active { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--disabled { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
.filled-button, .outlined-button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 40px; padding: 0 17px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-large); cursor: pointer; }
.filled-button { border: 1px solid var(--md-sys-color-primary); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); }
.outlined-button { border: 1px solid var(--md-sys-color-outline); background: transparent; color: var(--md-sys-color-primary); }
.outlined-button.small { min-height: 32px; padding: 0 11px; font: var(--md-sys-label-medium); }
.filled-button:disabled, .outlined-button:disabled { opacity: .5; cursor: not-allowed; }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
.icon-button:disabled { opacity: .5; cursor: not-allowed; }
@media (max-width: 620px) { .admin-form, .operation-credentials { grid-template-columns: 1fr; } }
</style>
