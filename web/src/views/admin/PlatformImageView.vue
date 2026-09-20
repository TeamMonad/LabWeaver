<template>
  <div class="platform-image-page">
    <header class="page-header">
      <div>
        <h2>平台镜像</h2>
        <p class="page-subtitle">维护沙箱可用的容器与虚拟机基础镜像。digest 是权威身份，tag 仅作解析入口。</p>
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
      @retry="images.load"
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
          <select v-model="registerForm.kind" class="text-input">
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
        <label>
          <span>信任版本</span>
          <input v-model.number="registerForm.trustRevision" class="text-input" type="number" min="1" required />
        </label>
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
          <h3 id="upload-heading">上传 OCI 归档</h3>
          <p>归档经预签名地址直传对象存储，由 Agent 校验每个 blob 后推送 registry 并登记目录。</p>
        </div>
      </div>
      <form class="admin-form" @submit.prevent="submitUpload">
        <label>
          <span>类型</span>
          <select v-model="uploadForm.kind" class="text-input">
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
        <label>
          <span>信任版本</span>
          <input v-model.number="uploadForm.trustRevision" class="text-input" type="number" min="1" required />
        </label>
        <label class="wide-field">
          <span>原因</span>
          <textarea v-model="uploadForm.reason" class="text-input" rows="2" maxlength="512" required />
        </label>
        <label class="wide-field">
          <span>OCI 归档（.tar）</span>
          <input ref="fileInput" class="text-input" type="file" accept=".tar" @change="selectFile" />
        </label>
        <button type="submit" class="filled-button" :disabled="busy || !uploadFile">上传并导入</button>
      </form>
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
import { usePlatformImages } from '@/composables/usePlatformImages'
import { formatBytes, truncateSha256 } from '@/utils/format'
import type { AsyncState, DiagnosticViewModel } from '@/types/async'
import type {
  PlatformImageEntryViewSchema,
  PlatformImageKind,
  PlatformImageStatus,
} from '@/generated/contracts'

type CatalogRow = PlatformImageEntryViewSchema & { actions?: never }

type PendingAction = { action: 'repin' | 'disable'; entry: PlatformImageEntryViewSchema }

const images = usePlatformImages()
const fileInput = ref<HTMLInputElement | null>(null)
const uploadFile = ref<File | null>(null)
const operationReason = ref('')
const repinTrustRevision = ref(1)
const pending = ref<PendingAction | null>(null)

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
}>({ kind: 'container', binding: '', targetReference: '', trustRevision: 1, reason: '' })

const catalogColumns: DataTableColumn<CatalogRow>[] = [
  { key: 'kind', title: '类型' },
  { key: 'binding', title: 'binding' },
  { key: 'sourceReference', title: '引用' },
  { key: 'resolvedDigest', title: 'digest' },
  { key: 'mediaType', title: '媒体类型' },
  { key: 'sizeBytes', title: '大小' },
  { key: 'status', title: '状态' },
  { key: 'trustRevision', title: '信任版本' },
  { key: 'repinGeneration', title: '重固定代次' },
  { key: 'releaseReferenceCount', title: '引用 release 数' },
  { key: 'actions', title: '操作' },
]

const busy = computed(() => images.state.kind === 'loading' || images.state.kind === 'uploading')
const actionReady = computed(() => operationReason.value.trim().length > 0)
const uploadProgress = computed(() => (images.state.kind === 'uploading' ? images.state.progress : null))

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
  uploadFile.value = selected && selected.length > 0 ? selected[0] : null
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
  const imported = await images.upload(file, { ...uploadForm })
  if (!imported) return
  uploadFile.value = null
  if (fileInput.value) fileInput.value.value = ''
  uploadForm.binding = ''
  uploadForm.targetReference = ''
  uploadForm.reason = ''
}

onMounted(() => images.load())
</script>

<style scoped>
.platform-image-page { display: grid; gap: 20px; }
.page-header, .section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.page-header h2, .section-heading h3 { margin: 0; color: var(--md-sys-color-on-surface); }
.page-header h2 { font: var(--md-sys-headline-small); }
.section-heading h3 { font: var(--md-sys-title-large); }
.page-subtitle, .section-heading p, .operation-hint, .upload-progress, .upload-file { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.catalog-card, .register-card, .upload-card { display: grid; gap: 16px; padding: 20px; }
.admin-form { display: grid; gap: 12px; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); align-items: end; }
.admin-form label, .operation-credentials label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.admin-form .wide-field { grid-column: 1 / -1; }
.admin-form button { justify-self: start; }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
textarea.text-input { resize: vertical; }
.operation-credentials { display: grid; gap: 12px; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); padding-top: 16px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.operation-hint { grid-column: 1 / -1; margin: 0; }
.row-actions { display: flex; gap: 8px; flex-wrap: wrap; }
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
