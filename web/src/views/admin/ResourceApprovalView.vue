<template>
  <div class="resource-approval">
    <header class="page-header">
      <h2>资源审批与 Lease 管理</h2>
      <p class="page-subtitle">审批平台资源申请，管理已签发的资源 Lease。所有变更均携带 revision fence 与幂等键。</p>
    </header>

    <DiagnosticBanner
      v-if="approval.outcome"
      :code="approval.outcome.diagnostic.code"
      :message="approval.outcome.diagnostic.message"
      :retryable="false"
      :severity="approval.outcome.kind === 'success' ? 'info' : 'error'"
    />

    <section class="request-section" aria-labelledby="request-heading">
      <h3 id="request-heading" class="section-title">
        <SvgIcon name="admin_panel_settings" size="sm" aria-hidden="true" />
        资源申请
      </h3>

      <div class="filter-row">
        <label class="filter-label" for="course-filter">课程过滤</label>
        <select id="course-filter" v-model="courseFilter" class="filter-select" aria-label="按课程过滤资源申请">
          <option value="">全部课程</option>
          <option v-for="courseId in courseOptions" :key="courseId" :value="courseId">{{ courseId }}</option>
        </select>
      </div>

      <DiagnosticBanner
        v-if="approval.requests.kind === 'error'"
        :code="approval.requests.diagnostic.code"
        :message="approval.requests.diagnostic.message"
        :retryable="approval.requests.diagnostic.retryable"
        severity="error"
        @retry="approval.load"
      />

      <DataTable
        v-else
        class="request-table"
        :columns="requestColumns"
        :rows="requestRows"
        :loading="approval.requests.kind === 'loading' || approval.requests.kind === 'idle'"
        empty-text="暂无资源申请"
        interactive
        aria-label="资源申请列表"
        @row-click="(row) => approval.selectRequest((row as unknown as RequestRow).id)"
      />

      <div v-if="approval.selectedRequest" class="request-detail md-card">
        <div class="detail-meta">
          <div class="meta-row">
            <span class="meta-label">申请 ID</span>
            <code class="meta-value">{{ approval.selectedRequest.id }}</code>
          </div>
          <div class="meta-row">
            <span class="meta-label">Request Key</span>
            <code class="meta-value">{{ approval.selectedRequest.requestKey }}</code>
          </div>
          <div class="meta-row">
            <span class="meta-label">申请人</span>
            <span class="meta-value">{{ approval.selectedRequest.requesterId }}</span>
          </div>
          <div class="meta-row">
            <span class="meta-label">课程 / 项目</span>
            <span class="meta-value">{{ approval.selectedRequest.courseId }} / {{ approval.selectedRequest.projectId ?? '—' }}</span>
          </div>
          <div class="meta-row">
            <span class="meta-label">目标环境</span>
            <code class="meta-value">{{ approval.selectedRequest.target.environmentId }}</code>
          </div>
          <div class="meta-row">
            <span class="meta-label">Release</span>
            <code class="meta-value">{{ approval.selectedRequest.target.releaseId }} · v{{ approval.selectedRequest.target.releaseVersion }} · {{ truncateSha256(approval.selectedRequest.target.releaseSha256) }}</code>
          </div>
          <div class="meta-row">
            <span class="meta-label">资源规格</span>
            <span class="meta-value">{{ formatResources(approval.selectedRequest.requestedResources) }}</span>
          </div>
          <div class="meta-row">
            <span class="meta-label">申请时长</span>
            <span class="meta-value">{{ formatDuration(approval.selectedRequest.requestedDurationSeconds) }}</span>
          </div>
          <div class="meta-row">
            <span class="meta-label">状态</span>
            <span class="meta-value">{{ approval.selectedRequest.state }}</span>
          </div>
          <div class="meta-row">
            <span class="meta-label">当前 Revision</span>
            <span class="meta-value">rev-{{ approval.selectedRequest.revision }}</span>
          </div>
          <div class="meta-row">
            <span class="meta-label">创建 / 更新</span>
            <span class="meta-value">{{ formatTimestamp(approval.selectedRequest.createdAt) }} / {{ formatTimestamp(approval.selectedRequest.updatedAt) }}</span>
          </div>
          <div v-if="approval.selectedRequest.diagnosticCode" class="meta-row">
            <span class="meta-label">Diagnostic</span>
            <code class="meta-value">{{ approval.selectedRequest.diagnosticCode }}</code>
          </div>
        </div>

        <div class="approval-controls">
          <textarea
            v-model="requestReason"
            class="reason-input"
            rows="2"
            maxlength="500"
            placeholder="审批 / 操作理由（必填，1-500 字）"
            aria-label="资源申请操作理由"
          />

          <template v-if="approval.selectedRequest.state === 'reviewing'">
            <div class="approve-inputs">
              <label class="input-label" for="provider-binding">Provider Binding</label>
              <input
                id="provider-binding"
                v-model="providerBinding"
                class="text-input"
                type="text"
                aria-label="Provider Binding"
              />
              <label class="input-label" for="approve-duration">批准时长（秒）</label>
              <input
                id="approve-duration"
                v-model.number="approveDuration"
                class="text-input"
                type="number"
                min="1"
                aria-label="批准时长（秒）"
              />
            </div>

            <fieldset v-if="resizeMode" class="resize-fieldset">
              <legend>调整后资源规格</legend>
              <div class="approve-inputs">
                <label class="input-label" for="resize-cpu">CPU（millicores）</label>
                <input id="resize-cpu" v-model.number="resizeCpuMillicores" class="text-input" type="number" min="1" aria-label="调整后 CPU millicores" />
                <label class="input-label" for="resize-memory">内存（GiB）</label>
                <input id="resize-memory" v-model.number="resizeMemoryGiB" class="text-input" type="number" min="1" aria-label="调整后内存 GiB" />
                <label class="input-label" for="resize-storage">存储（GiB）</label>
                <input id="resize-storage" v-model.number="resizeStorageGiB" class="text-input" type="number" min="1" aria-label="调整后存储 GiB" />
                <label class="input-label" for="resize-gpu-class">GPU 类别（留空表示无 GPU）</label>
                <input id="resize-gpu-class" v-model="resizeGpuClass" class="text-input" type="text" aria-label="调整后 GPU 类别" />
                <label class="input-label" for="resize-gpu-count">GPU 数量</label>
                <input id="resize-gpu-count" v-model.number="resizeGpuCount" class="text-input" type="number" min="0" aria-label="调整后 GPU 数量" />
              </div>
            </fieldset>

            <div class="approval-buttons">
              <button
                type="button"
                class="filled-button"
                :disabled="!canSubmitRequestAction || approval.acting !== null"
                @click="openRequestConfirm('approve')"
              >
                批准
              </button>
              <button
                type="button"
                class="outlined-button"
                :disabled="!canSubmitRequestAction || approval.acting !== null"
                @click="onResizeClick"
              >
                {{ resizeMode ? '确认调整并批准' : '调整并批准' }}
              </button>
              <button
                type="button"
                class="outlined-button"
                :disabled="!validReason || approval.acting !== null"
                @click="openRequestConfirm('reject')"
              >
                拒绝
              </button>
            </div>
          </template>

          <div v-else class="approval-buttons">
            <button
              v-if="approval.selectedRequest.state === 'allocating'"
              type="button"
              class="outlined-button"
              :disabled="!validReason || approval.acting !== null"
              @click="openRequestConfirm('retry')"
            >
              重试分配
            </button>
            <p v-else class="state-note">当前状态 {{ approval.selectedRequest.state }} 为只读，无可用管理操作。</p>
          </div>
        </div>
      </div>
      <p v-else class="select-hint">选择一条申请查看详情并执行审批操作。</p>
    </section>

    <section class="lease-section" aria-labelledby="lease-heading">
      <h3 id="lease-heading" class="section-title">
        <SvgIcon name="environment" size="sm" aria-hidden="true" />
        资源 Lease
      </h3>

      <DiagnosticBanner
        v-if="approval.leases.kind === 'error'"
        :code="approval.leases.diagnostic.code"
        :message="approval.leases.diagnostic.message"
        :retryable="approval.leases.diagnostic.retryable"
        severity="error"
        @retry="approval.load"
      />

      <DataTable
        v-else
        class="lease-table"
        :columns="leaseColumns"
        :rows="leaseRows"
        :loading="approval.leases.kind === 'loading' || approval.leases.kind === 'idle'"
        empty-text="暂无资源 Lease"
        interactive
        aria-label="资源 Lease 列表"
        @row-click="(row) => approval.selectLease((row as unknown as LeaseRow).id)"
      />

      <div v-if="approval.selectedLease" class="lease-detail md-card">
        <div class="detail-meta">
          <div class="meta-row">
            <span class="meta-label">Lease ID</span>
            <code class="meta-value">{{ approval.selectedLease.id }}</code>
          </div>
          <div class="meta-row">
            <span class="meta-label">申请 ID</span>
            <code class="meta-value">{{ approval.selectedLease.requestId }}</code>
          </div>
          <div class="meta-row">
            <span class="meta-label">Claim ID</span>
            <code class="meta-value">{{ approval.selectedLease.claimId }}</code>
          </div>
          <div class="meta-row">
            <span class="meta-label">状态</span>
            <span class="meta-value">{{ approval.selectedLease.state }}</span>
          </div>
          <div class="meta-row">
            <span class="meta-label">当前 Revision</span>
            <span class="meta-value">rev-{{ approval.selectedLease.revision }}</span>
          </div>
          <div class="meta-row">
            <span class="meta-label">Active From</span>
            <span class="meta-value">{{ approval.selectedLease.activeFrom ? formatTimestamp(approval.selectedLease.activeFrom) : '—' }}</span>
          </div>
          <div class="meta-row">
            <span class="meta-label">Expires At</span>
            <span class="meta-value">{{ approval.selectedLease.expiresAt ? formatTimestamp(approval.selectedLease.expiresAt) : '—' }}</span>
          </div>
          <div v-if="approval.selectedLease.revokeReasonCode" class="meta-row">
            <span class="meta-label">撤销原因码</span>
            <code class="meta-value">{{ approval.selectedLease.revokeReasonCode }}</code>
          </div>
        </div>

        <div class="approval-controls">
          <div class="approve-inputs">
            <label class="input-label" for="renew-duration">续期时长（秒）</label>
            <input
              id="renew-duration"
              v-model.number="renewDuration"
              class="text-input"
              type="number"
              min="1"
              aria-label="续期时长（秒）"
            />
          </div>
          <textarea
            v-model="leaseReason"
            class="reason-input"
            rows="2"
            maxlength="500"
            placeholder="续期 / 撤销理由（必填，1-500 字）"
            aria-label="Lease 操作理由"
          />
          <div class="approval-buttons">
            <button
              v-if="approval.selectedLease.state === 'active' || approval.selectedLease.state === 'expiring'"
              type="button"
              class="filled-button"
              :disabled="!validLeaseReason || !validRenewDuration || approval.acting !== null"
              @click="openLeaseConfirm('renew')"
            >
              续期
            </button>
            <button
              v-if="approval.selectedLease.state !== 'expired' && approval.selectedLease.state !== 'revoked'"
              type="button"
              class="outlined-button"
              :disabled="!validLeaseReason || approval.acting !== null"
              @click="openLeaseConfirm('revoke')"
            >
              撤销 Lease
            </button>
            <p v-if="approval.selectedLease.state === 'expired' || approval.selectedLease.state === 'revoked'" class="state-note">
              当前状态 {{ approval.selectedLease.state }} 为终态，无可用管理操作。
            </p>
          </div>
        </div>
      </div>
      <p v-else class="select-hint">选择一条 Lease 执行续期或撤销。</p>
    </section>

    <ConfirmDialog
      :open="pendingRequestAction !== null"
      :title="requestConfirmTitle"
      :description="requestConfirmDescription"
      confirm-text="确认"
      severity="warning"
      @confirm="onRequestConfirmed"
      @cancel="pendingRequestAction = null"
    />

    <ConfirmDialog
      :open="pendingLeaseAction !== null"
      :title="leaseConfirmTitle"
      :description="leaseConfirmDescription"
      confirm-text="确认"
      :severity="pendingLeaseAction === 'revoke' ? 'error' : 'warning'"
      @confirm="onLeaseConfirmed"
      @cancel="pendingLeaseAction = null"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import DataTable, { type DataTableColumn } from '@/components/common/DataTable.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useResourceApproval, type LeaseActionKind, type RequestActionKind } from '@/composables/useResourceApproval'
import type { WorkloadResources } from '@/generated/contracts'
import { formatBytes, formatTimestamp, truncateSha256 } from '@/utils/format'

const GIB = 1024 ** 3
const DEFAULT_PROVIDER_BINDING = 'mock-capacity-primary'

const approval = useResourceApproval()

interface RequestRow extends Record<string, unknown> {
  id: string
  requestKey: string
  environmentId: string
  releaseVersion: string
  resources: string
  duration: string
  state: string
  revision: string
  updatedAt: string
}

interface LeaseRow extends Record<string, unknown> {
  id: string
  requestId: string
  claimId: string
  resources: string
  state: string
  revision: string
  activeFrom: string
  expiresAt: string
}

function formatResources(resources: WorkloadResources): string {
  const base = `${resources.cpuMillicores}m CPU · ${formatBytes(resources.memoryBytes)} 内存 · ${formatBytes(resources.storageBytes)} 存储`
  return resources.gpu ? `${base} · ${resources.gpu.class} × ${resources.gpu.count}` : base
}

function formatDuration(seconds: number): string {
  if (seconds % 3600 === 0) return `${seconds / 3600} 小时`
  if (seconds % 60 === 0) return `${seconds / 60} 分钟`
  return `${seconds} 秒`
}

const courseFilter = ref('')

const courseOptions = computed(() => {
  if (approval.requests.kind !== 'success') return []
  return Array.from(new Set(approval.requests.data.map((request) => request.courseId))).sort()
})

const requestColumns: DataTableColumn<RequestRow>[] = [
  { key: 'requestKey', title: '申请标识' },
  { key: 'environmentId', title: '环境' },
  { key: 'releaseVersion', title: 'Release 版本' },
  { key: 'resources', title: '资源规格' },
  { key: 'duration', title: '时长' },
  { key: 'state', title: '状态' },
  { key: 'revision', title: 'Revision' },
  { key: 'updatedAt', title: '更新时间' },
]

const requestRows = computed<RequestRow[]>(() => {
  if (approval.requests.kind !== 'success') return []
  return approval.requests.data
    .filter((request) => !courseFilter.value || request.courseId === courseFilter.value)
    .map((request) => ({
      id: request.id,
      requestKey: request.requestKey,
      environmentId: request.target.environmentId,
      releaseVersion: `v${request.target.releaseVersion}`,
      resources: formatResources(request.requestedResources),
      duration: formatDuration(request.requestedDurationSeconds),
      state: request.state,
      revision: `rev-${request.revision}`,
      updatedAt: formatTimestamp(request.updatedAt),
    }))
})

const leaseColumns: DataTableColumn<LeaseRow>[] = [
  { key: 'id', title: 'Lease ID' },
  { key: 'requestId', title: '申请 ID' },
  { key: 'resources', title: '资源规格' },
  { key: 'state', title: '状态' },
  { key: 'revision', title: 'Revision' },
  { key: 'activeFrom', title: 'Active From' },
  { key: 'expiresAt', title: 'Expires At' },
]

const leaseRows = computed<LeaseRow[]>(() => {
  if (approval.leases.kind !== 'success') return []
  return approval.leases.data.map((lease) => {
    const resources = approval.requestResources.get(lease.requestId)
    return {
      id: lease.id,
      requestId: lease.requestId,
      claimId: lease.claimId,
      resources: resources ? formatResources(resources) : '—',
      state: lease.state,
      revision: `rev-${lease.revision}`,
      activeFrom: lease.activeFrom ? formatTimestamp(lease.activeFrom) : '—',
      expiresAt: lease.expiresAt ? formatTimestamp(lease.expiresAt) : '—',
    }
  })
})

const requestReason = ref('')
const providerBinding = ref(DEFAULT_PROVIDER_BINDING)
const approveDuration = ref(7200)
const resizeMode = ref(false)
const resizeCpuMillicores = ref(2000)
const resizeMemoryGiB = ref(4)
const resizeStorageGiB = ref(20)
const resizeGpuClass = ref('')
const resizeGpuCount = ref(0)

const validReason = computed(() => {
  const length = requestReason.value.trim().length
  return length >= 1 && length <= 500
})

const validApproveInputs = computed(
  () => providerBinding.value.trim().length > 0 && Number.isInteger(approveDuration.value) && approveDuration.value > 0,
)

const validResizeInputs = computed(
  () =>
    Number.isInteger(resizeCpuMillicores.value) && resizeCpuMillicores.value > 0 &&
    Number.isInteger(resizeMemoryGiB.value) && resizeMemoryGiB.value > 0 &&
    Number.isInteger(resizeStorageGiB.value) && resizeStorageGiB.value > 0 &&
    (resizeGpuClass.value.trim() === '' || (Number.isInteger(resizeGpuCount.value) && resizeGpuCount.value > 0)),
)

const canSubmitRequestAction = computed(
  () => validReason.value && validApproveInputs.value && (!resizeMode.value || validResizeInputs.value),
)

function resizeResources(): WorkloadResources {
  const gpuClass = resizeGpuClass.value.trim()
  return {
    cpuMillicores: resizeCpuMillicores.value,
    memoryBytes: resizeMemoryGiB.value * GIB,
    storageBytes: resizeStorageGiB.value * GIB,
    ...(gpuClass ? { gpu: { class: gpuClass, count: resizeGpuCount.value } } : {}),
  }
}

watch(
  () => approval.selectedRequest,
  (request) => {
    requestReason.value = ''
    resizeMode.value = false
    providerBinding.value = DEFAULT_PROVIDER_BINDING
    if (!request) return
    approveDuration.value = request.requestedDurationSeconds
    resizeCpuMillicores.value = request.requestedResources.cpuMillicores
    resizeMemoryGiB.value = Math.round(request.requestedResources.memoryBytes / GIB)
    resizeStorageGiB.value = Math.round(request.requestedResources.storageBytes / GIB)
    resizeGpuClass.value = request.requestedResources.gpu?.class ?? ''
    resizeGpuCount.value = request.requestedResources.gpu?.count ?? 0
  },
)

const leaseReason = ref('')
const renewDuration = ref(7200)

const validLeaseReason = computed(() => {
  const length = leaseReason.value.trim().length
  return length >= 1 && length <= 500
})

const validRenewDuration = computed(() => Number.isInteger(renewDuration.value) && renewDuration.value > 0)

watch(
  () => approval.selectedLease,
  () => {
    leaseReason.value = ''
  },
)

const pendingRequestAction = ref<RequestActionKind | null>(null)
const pendingLeaseAction = ref<LeaseActionKind | null>(null)

const requestConfirmTitle = computed(() => {
  switch (pendingRequestAction.value) {
    case 'approve':
      return '确认批准资源申请'
    case 'resize':
      return '确认调整并批准资源申请'
    case 'reject':
      return '确认拒绝资源申请'
    case 'retry':
      return '确认重试分配'
    default:
      return ''
  }
})

const requestConfirmDescription = computed(() => {
  const request = approval.selectedRequest
  if (!request || !pendingRequestAction.value) return ''
  const base = `将对申请 ${request.requestKey}（rev-${request.revision}）执行操作，理由：${requestReason.value.trim()}`
  return pendingRequestAction.value === 'resize' ? `${base}。调整后规格：${formatResources(resizeResources())}。` : `${base}。`
})

function openRequestConfirm(kind: RequestActionKind) {
  pendingRequestAction.value = kind
}

function onResizeClick() {
  if (!resizeMode.value) {
    resizeMode.value = true
    return
  }
  openRequestConfirm('resize')
}

async function onRequestConfirmed() {
  const kind = pendingRequestAction.value
  const request = approval.selectedRequest
  pendingRequestAction.value = null
  if (!kind || !request) return
  const ok = await approval.runRequestAction(kind, request.id, {
    providerBinding: providerBinding.value.trim(),
    resources: kind === 'resize' ? resizeResources() : request.requestedResources,
    durationSeconds: approveDuration.value,
    reason: requestReason.value.trim(),
  })
  if (ok) {
    requestReason.value = ''
    resizeMode.value = false
  }
}

const leaseConfirmTitle = computed(() =>
  pendingLeaseAction.value === 'renew' ? '确认续期 Lease' : pendingLeaseAction.value === 'revoke' ? '确认撤销 Lease' : '',
)

const leaseConfirmDescription = computed(() => {
  const lease = approval.selectedLease
  if (!lease || !pendingLeaseAction.value) return ''
  const base = `将对 Lease ${lease.id}（rev-${lease.revision}）执行操作，理由：${leaseReason.value.trim()}`
  return pendingLeaseAction.value === 'renew' ? `${base}。续期时长：${formatDuration(renewDuration.value)}。` : `${base}。撤销后访问立即失效。`
})

function openLeaseConfirm(kind: LeaseActionKind) {
  pendingLeaseAction.value = kind
}

async function onLeaseConfirmed() {
  const kind = pendingLeaseAction.value
  const lease = approval.selectedLease
  pendingLeaseAction.value = null
  if (!kind || !lease) return
  const ok = kind === 'renew'
    ? await approval.renewLease(lease.id, renewDuration.value, leaseReason.value.trim())
    : await approval.revokeLease(lease.id, leaseReason.value.trim())
  if (ok) {
    leaseReason.value = ''
  }
}
</script>

<style scoped>
.resource-approval {
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

.filter-row {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
}

.filter-label {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.filter-select {
  height: 36px;
  padding: 0 12px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
}

.request-detail,
.lease-detail {
  margin-top: 16px;
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.meta-row {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 8px 0;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.meta-row:last-child {
  border-bottom: none;
}

.meta-label {
  width: 140px;
  flex-shrink: 0;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.meta-value {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface);
  word-break: break-all;
}

.approval-controls {
  margin-top: 20px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.approve-inputs {
  display: grid;
  grid-template-columns: 180px minmax(0, 1fr);
  align-items: center;
  gap: 8px 12px;
}

.input-label {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.text-input {
  height: 36px;
  padding: 0 12px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
}

.resize-fieldset {
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  padding: 12px;
}

.resize-fieldset legend {
  padding: 0 8px;
  font: var(--md-sys-title-small);
  color: var(--md-sys-color-on-surface-variant);
}

.reason-input {
  width: 100%;
  padding: 12px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
  resize: vertical;
}

.approval-buttons {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
  align-items: center;
}

.filled-button,
.outlined-button {
  height: 40px;
  padding: 0 24px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.filled-button {
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
}

.filled-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.outlined-button {
  border: 1px solid var(--md-sys-color-outline);
  background: transparent;
  color: var(--md-sys-color-primary);
}

.outlined-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.state-note,
.select-hint {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  margin: 12px 0 0;
}
</style>
