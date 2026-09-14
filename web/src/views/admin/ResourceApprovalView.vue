<template>
  <div class="resource-approval">
    <header class="page-header">
      <h2>资源审批与 Lease 管理</h2>
      <p class="page-subtitle">
        审批平台资源申请，管理已签发的资源 Lease。所有变更均携带 revision fence 与幂等键。
      </p>
    </header>

    <DiagnosticBanner
      v-if="approval.outcome"
      :code="approval.outcome.diagnostic.code"
      :message="approval.outcome.diagnostic.message"
      :retryable="false"
      :severity="approval.outcome.kind === 'success' ? 'info' : 'error'"
    />

    <DiagnosticBanner
      v-if="approval.refreshDiagnostic"
      :code="approval.refreshDiagnostic.code"
      :message="approval.refreshDiagnostic.message"
      :retryable="approval.refreshDiagnostic.retryable"
      severity="warning"
      @retry="approval.load"
    />

    <section
      class="request-section"
      aria-labelledby="request-heading"
    >
      <h3
        id="request-heading"
        class="section-title"
      >
        <SvgIcon
          name="admin_panel_settings"
          size="sm"
          aria-hidden="true"
        />
        资源申请
      </h3>

      <div class="filter-row">
        <label
          class="filter-label"
          for="course-filter"
        >课程过滤</label>
        <select
          id="course-filter"
          v-model="courseFilter"
          class="filter-select"
          aria-label="按课程过滤资源申请"
        >
          <option value="">
            全部课程
          </option>
          <option
            v-for="courseId in courseOptions"
            :key="courseId"
            :value="courseId"
          >
            {{ courseId }}
          </option>
        </select>
        <label
          class="filter-label"
          for="request-search"
        >请求搜索</label>
        <input
          id="request-search"
          v-model="requestSearch"
          class="text-input filter-input"
          type="search"
          placeholder="ID、Request Key、申请人、项目或目标"
          aria-label="按真实请求字段搜索"
        >
        <label
          class="filter-label"
          for="request-state-filter"
        >状态</label>
        <select
          id="request-state-filter"
          v-model="requestStateFilter"
          class="filter-select"
          aria-label="按资源申请状态过滤"
        >
          <option value="">
            全部状态
          </option>
          <option
            v-for="state in requestStateOptions"
            :key="state"
            :value="state"
          >
            {{ resourceRequestStateLabel(state) }}
          </option>
        </select>
      </div>

      <p class="filter-hint">
        筛选只匹配服务端返回的 ID、Request Key、申请人、课程、项目和目标标识；公开请求没有可靠的 submission 关联字段，不按名称推断关联。
      </p>

      <DiagnosticBanner
        v-if="approval.requests.kind === 'error'"
        :code="approval.requests.diagnostic.code"
        :message="approval.requests.diagnostic.message"
        :retryable="approval.requests.diagnostic.retryable"
        severity="error"
        @retry="approval.load"
      />

      <!-- @vue-generic {RequestRow} -->
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
      >
        <template #state="{ row }">
          <GcpStatusPill
            :state="row.state"
            domain="resource"
          />
        </template>
        <template #selection="{ row }">
          <input
            v-if="isBatchSelectable(row)"
            type="checkbox"
            :checked="selectedRequestIds.includes(row.id)"
            :aria-label="`选择任务请求 ${row.requestKey}`"
            @click.stop
            @change="toggleRequestSelection(row.id)"
          >
          <span
            v-else
            class="selection-unavailable"
            title="只支持明确选择待审批的任务请求"
          >—</span>
        </template>
      </DataTable>

      <div
        v-if="requestRows.some((row) => row.targetKind === 'task') || approval.batchOutcome"
        class="batch-approval-panel md-card"
        aria-labelledby="batch-approval-heading"
      >
        <div class="batch-approval-heading">
          <div>
            <h4 id="batch-approval-heading">
              批量批准明确选中的任务请求
            </h4>
            <p>
              当前 API 没有 submission 关联契约，因此只对你勾选的真实 task 请求逐项调用批准接口；每项会读取最新 revision 和状态，失败项会单独保留。
            </p>
          </div>
          <span
            class="batch-selection-count"
            role="status"
          >
            已选择 {{ selectedBatchRequests.length }} 项
          </span>
        </div>

        <div class="batch-approval-fields">
          <label for="batch-provider-binding">执行后端绑定</label>
          <input
            id="batch-provider-binding"
            v-model="batchProviderBinding"
            class="text-input"
            type="text"
            maxlength="120"
            placeholder="填写所有选中请求可用的 provider binding"
            aria-label="批量审批执行后端绑定"
          >
          <label for="batch-approval-reason">批量审批理由</label>
          <textarea
            id="batch-approval-reason"
            v-model="batchReason"
            class="reason-input"
            rows="2"
            maxlength="500"
            placeholder="批量审批理由（必填，1-500 字）"
            aria-label="批量资源申请操作理由"
          />
        </div>
        <p
          class="batch-approval-hint"
          role="note"
        >
          <template v-if="selectedBatchRequests.length === 0">
            先勾选状态为“待审批”的 task 请求；环境请求与其他状态不能批量审批。
          </template>
          <template v-else-if="!validBatchProviderBinding">
            当前绑定不是所有已选请求可用的真实 provider binding；GPU 请求必须匹配当前容量目录。
          </template>
          <template v-else>
            将按列表中的每个请求原始资源规格和申请时长提交；不会按 Request Key 或名称猜测 submission 关系。
          </template>
        </p>
        <div class="approval-buttons">
          <button
            type="button"
            class="filled-button"
            :disabled="!canBatchApprove || approval.acting !== null"
            @click="openBatchConfirm"
          >
            批准已选择的 {{ selectedBatchRequests.length }} 项
          </button>
          <button
            type="button"
            class="text-button"
            :disabled="selectedRequestIds.length === 0 || approval.acting !== null"
            @click="clearRequestSelection"
          >
            清除选择
          </button>
        </div>

        <div
          v-if="approval.batchOutcome"
          class="batch-outcome"
          role="status"
        >
          <strong>批量审批结果：{{ batchOutcomeLabel(approval.batchOutcome.kind) }}</strong>
          <ul>
            <li
              v-for="item in approval.batchOutcome.items"
              :key="item.requestId"
              :class="`batch-outcome-item batch-outcome-item--${item.kind}`"
            >
              <code>{{ batchRequestLabel(item.requestId) }}</code>
              <span>{{ item.diagnostic.message }}</span>
              <code>{{ item.diagnostic.code }}</code>
            </li>
          </ul>
        </div>
      </div>

      <div
        v-if="approval.selectedRequest"
        class="request-detail md-card"
      >
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
            <span class="meta-label">目标</span>
            <code class="meta-value">{{ targetEnvironment(approval.selectedRequest.target) }}</code>
          </div>
          <div class="meta-row">
            <span class="meta-label">Release</span>
            <code class="meta-value">{{ targetRelease(approval.selectedRequest.target) }}</code>
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
            <GcpStatusPill
              :state="approval.selectedRequest.state"
              domain="resource"
            />
          </div>
          <div class="meta-row">
            <span class="meta-label">当前 Revision</span>
            <span class="meta-value">rev-{{ approval.selectedRequest.revision }}</span>
          </div>
          <div class="meta-row">
            <span class="meta-label">创建 / 更新</span>
            <span class="meta-value">{{ formatTimestamp(approval.selectedRequest.createdAt) }} / {{ formatTimestamp(approval.selectedRequest.updatedAt) }}</span>
          </div>
          <div
            v-if="approval.selectedRequest.diagnosticCode"
            class="meta-row"
          >
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
              <template v-if="requiresGpuProvider">
                <label
                  class="input-label"
                  for="provider-binding"
                >GPU Provider Binding</label>
                <select
                  id="provider-binding"
                  v-model="providerBinding"
                  class="text-input"
                  aria-label="GPU Provider Binding"
                  :disabled="approval.providerOptions.kind !== 'success' || eligibleProviderOptions.length === 0"
                  aria-describedby="provider-binding-hint"
                >
                  <option value="">
                    选择 Resource GPU 提供方
                  </option>
                  <option
                    v-for="option in eligibleProviderOptions"
                    :key="option.providerBinding"
                    :value="option.providerBinding"
                  >
                    {{ option.providerBinding }} · {{ option.gpuClasses.length ? `GPU: ${option.gpuClasses.join('、')}` : '无 GPU 项目' }}
                  </option>
                </select>
              </template>
              <template v-else>
                <label
                  class="input-label"
                  for="provider-binding"
                >执行后端绑定（CPU 必填）</label>
                <input
                  id="provider-binding"
                  v-model="providerBinding"
                  class="text-input"
                  type="text"
                  maxlength="120"
                  placeholder="填写当前 Environment 可用的 provider binding"
                  aria-label="执行后端绑定"
                  aria-describedby="provider-binding-hint"
                >
              </template>
              <p
                id="provider-binding-hint"
                class="provider-binding-hint"
                role="note"
              >
                <template v-if="approval.providerOptions.kind === 'loading'">
                  正在读取 Resource 容量目录。
                </template>
                <template v-else-if="approval.providerOptions.kind === 'error'">
                  {{ approval.providerOptions.diagnostic.message }}
                </template>
                <template v-else-if="requiresGpuProvider && eligibleProviderOptions.length === 0">
                  当前 GPU 请求没有匹配的真实容量提供方，无法审批。
                </template>
                <template v-else-if="requiresGpuProvider">
                  GPU 提供方必须来自 Resource 当前 GPU 目录。
                </template>
                <template v-else>
                  CPU-only 请求不依赖 GPU 目录；填写已在 Environment 配置中启用的执行后端绑定。
                </template>
              </p>
              <label
                class="input-label"
                for="approve-duration"
              >批准时长（秒）</label>
              <input
                id="approve-duration"
                v-model.number="approveDuration"
                class="text-input"
                type="number"
                min="1"
                aria-label="批准时长（秒）"
              >
            </div>

            <fieldset
              v-if="resizeMode"
              class="resize-fieldset"
            >
              <legend>调整后资源规格</legend>
              <div class="approve-inputs">
                <label
                  class="input-label"
                  for="resize-cpu"
                >CPU（millicores）</label>
                <input
                  id="resize-cpu"
                  v-model.number="resizeCpuMillicores"
                  class="text-input"
                  type="number"
                  min="1"
                  aria-label="调整后 CPU millicores"
                >
                <label
                  class="input-label"
                  for="resize-memory"
                >内存（GiB）</label>
                <input
                  id="resize-memory"
                  v-model.number="resizeMemoryGiB"
                  class="text-input"
                  type="number"
                  min="1"
                  aria-label="调整后内存 GiB"
                >
                <label
                  class="input-label"
                  for="resize-storage"
                >存储（GiB）</label>
                <input
                  id="resize-storage"
                  v-model.number="resizeStorageGiB"
                  class="text-input"
                  type="number"
                  min="1"
                  aria-label="调整后存储 GiB"
                >
                <label
                  class="input-label"
                  for="resize-gpu-class"
                >GPU 类别（留空表示无 GPU）</label>
                <input
                  id="resize-gpu-class"
                  v-model="resizeGpuClass"
                  class="text-input"
                  type="text"
                  aria-label="调整后 GPU 类别"
                >
                <label
                  class="input-label"
                  for="resize-gpu-count"
                >GPU 数量</label>
                <input
                  id="resize-gpu-count"
                  v-model.number="resizeGpuCount"
                  class="text-input"
                  type="number"
                  min="0"
                  aria-label="调整后 GPU 数量"
                >
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

          <div
            v-else
            class="approval-buttons"
          >
            <button
              v-if="approval.selectedRequest.state === 'allocating'"
              type="button"
              class="outlined-button"
              :disabled="!validReason || approval.acting !== null"
              @click="openRequestConfirm('retry')"
            >
              重试分配
            </button>
            <p
              v-else
              class="state-note"
            >
              当前状态 {{ resourceRequestStateLabel(approval.selectedRequest.state) }} 为只读，无可用管理操作。
            </p>
          </div>
        </div>
      </div>
      <p
        v-else
        class="select-hint"
      >
        选择一条申请查看详情并执行审批操作。
      </p>
    </section>

    <section
      class="lease-section"
      aria-labelledby="lease-heading"
    >
      <h3
        id="lease-heading"
        class="section-title"
      >
        <SvgIcon
          name="environment"
          size="sm"
          aria-hidden="true"
        />
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

      <!-- @vue-generic {LeaseRow} -->
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
      >
        <template #state="{ row }">
          <GcpStatusPill
            :state="row.state"
            domain="resource"
          />
        </template>
      </DataTable>

      <div
        v-if="approval.selectedLease"
        class="lease-detail md-card"
      >
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
            <GcpStatusPill
              :state="approval.selectedLease.state"
              domain="resource"
            />
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
          <div
            v-if="approval.selectedLease.revokeReasonCode"
            class="meta-row"
          >
            <span class="meta-label">撤销原因码</span>
            <code class="meta-value">{{ approval.selectedLease.revokeReasonCode }}</code>
          </div>
        </div>

        <div class="approval-controls">
          <div class="approve-inputs">
            <label
              class="input-label"
              for="renew-duration"
            >续期时长（秒）</label>
            <input
              id="renew-duration"
              v-model.number="renewDuration"
              class="text-input"
              type="number"
              min="1"
              aria-label="续期时长（秒）"
            >
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
            <p
              v-if="approval.selectedLease.state === 'expired' || approval.selectedLease.state === 'revoked'"
              class="state-note"
            >
              当前状态 {{ resourceLeaseStateLabel(approval.selectedLease.state) }} 为终态，无可用管理操作。
            </p>
          </div>
        </div>
      </div>
      <p
        v-else
        class="select-hint"
      >
        选择一条 Lease 执行续期或撤销。
      </p>
    </section>

    <ConfirmDialog
      :open="pendingRequestAction !== null"
      :title="requestConfirmTitle"
      :description="requestConfirmDescription"
      confirm-text="确认"
      severity="warning"
      @confirm="onRequestConfirmed"
      @cancel="cancelRequestConfirm"
    />

    <ConfirmDialog
      :open="pendingBatchApproval"
      title="确认批量批准任务请求"
      :description="batchConfirmDescription"
      confirm-text="确认批量批准"
      severity="warning"
      @confirm="onBatchConfirmed"
      @cancel="cancelBatchConfirm"
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
import GcpStatusPill from '@/components/common/GcpStatusPill.vue'
import { requestFingerprint, useResourceApproval, type LeaseActionKind, type RequestActionKind, type BatchActionOutcome, type RequestActionItem } from '@/composables/useResourceApproval'
import type { ResourceRequestSchema, ResourceRequestSchemaResourceTarget, ResourceRequestState, WorkloadResources } from '@/generated/contracts'
import { formatBytes, formatTimestamp } from '@/utils/format'
import { resourceRequestStateLabel, resourceLeaseStateLabel } from '@/utils/stateLabels'

const GIB = 1024 ** 3
const DEFAULT_PROVIDER_BINDING = ''

const approval = useResourceApproval()

interface RequestRow extends Record<string, unknown> {
  id: string
  selection: string
  requestKey: string
  environmentId: string
  releaseVersion: string
  resources: string
  duration: string
  state: string
  revision: string
  updatedAt: string
  targetKind: ResourceRequestSchema['target']['kind']
  stateValue: ResourceRequestState
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

function copyResources(resources: WorkloadResources): WorkloadResources {
  return {
    ...resources,
    ...(resources.gpu ? { gpu: { ...resources.gpu } } : {}),
  }
}

function formatDuration(seconds: number): string {
  if (seconds % 3600 === 0) return `${seconds / 3600} 小时`
  if (seconds % 60 === 0) return `${seconds / 60} 分钟`
  return `${seconds} 秒`
}

function targetEnvironment(target: ResourceRequestSchemaResourceTarget): string {
  return target.kind === 'environment' ? target.environmentId : `TaskRun ${target.taskRunId}`
}

function targetRelease(target: ResourceRequestSchemaResourceTarget): string {
  return target.kind === 'environment' ? `${target.releaseId} · v${target.releaseVersion}` : '—'
}

const courseFilter = ref('')
const requestSearch = ref('')
const requestStateFilter = ref<ResourceRequestState | ''>('')
const selectedRequestIds = ref<string[]>([])
const batchProviderBinding = ref('')
const batchReason = ref('')
const pendingBatchApproval = ref(false)

const courseOptions = computed(() => {
  if (approval.requests.kind !== 'success') return []
  return Array.from(new Set(
    approval.requests.data
      .map((request) => request.courseId)
      .filter((courseId): courseId is string => typeof courseId === 'string'),
  )).sort()
})

const requestColumns: DataTableColumn<RequestRow>[] = [
  { key: 'selection', title: '选择任务', width: '100px' },
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
  const query = requestSearch.value.trim().toLocaleLowerCase()
  return approval.requests.data
    .filter((request) => !courseFilter.value || request.courseId === courseFilter.value)
    .filter((request) => !requestStateFilter.value || request.state === requestStateFilter.value)
    .filter((request) => !query || requestSearchFields(request).some((field) => field.toLocaleLowerCase().includes(query)))
    .map((request) => ({
      id: request.id,
      selection: '',
      requestKey: request.requestKey,
      environmentId: targetEnvironment(request.target),
      releaseVersion: request.target.kind === 'environment' ? `v${request.target.releaseVersion}` : '—',
      resources: formatResources(request.requestedResources),
      duration: formatDuration(request.requestedDurationSeconds),
      state: resourceRequestStateLabel(request.state),
      revision: `rev-${request.revision}`,
      updatedAt: formatTimestamp(request.updatedAt),
      targetKind: request.target.kind,
      stateValue: request.state,
    }))
})

const requestStateOptions = computed<ResourceRequestState[]>(() => {
  if (approval.requests.kind !== 'success') return []
  return Array.from(new Set(approval.requests.data.map((request) => request.state))).sort()
})

function requestSearchFields(request: ResourceRequestSchema): string[] {
  const target = request.target.kind === 'environment'
    ? [request.target.kind, request.target.environmentId, request.target.releaseId, String(request.target.releaseVersion)]
    : [request.target.kind, request.target.taskRunId]
  return [
    request.id,
    request.requestKey,
    request.requesterId,
    request.courseId ?? '',
    request.projectId,
    ...target,
  ]
}

function requestById(requestId: string): ResourceRequestSchema | null {
  if (approval.requests.kind !== 'success') return null
  return approval.requests.data.find((request) => request.id === requestId) ?? null
}

function isBatchSelectable(row: RequestRow): boolean {
  const request = requestById(row.id)
  return request?.target.kind === 'task' && request.state === 'reviewing'
}

const selectedBatchRequests = computed<ResourceRequestSchema[]>(() => selectedRequestIds.value
  .map((requestId) => requestById(requestId))
  .filter((request): request is ResourceRequestSchema => request !== null
    && request.target.kind === 'task'
    && request.state === 'reviewing'))

watch(
  () => approval.requests.kind === 'success'
    ? approval.requests.data
      .filter((request) => request.target.kind === 'task' && request.state === 'reviewing')
      .map((request) => request.id)
    : [],
  (availableIds) => {
    const available = new Set(availableIds)
    selectedRequestIds.value = selectedRequestIds.value.filter((requestId) => available.has(requestId))
  },
  { immediate: true },
)

function toggleRequestSelection(requestId: string) {
  const request = requestById(requestId)
  if (!request || request.target.kind !== 'task' || request.state !== 'reviewing') return
  selectedRequestIds.value = selectedRequestIds.value.includes(requestId)
    ? selectedRequestIds.value.filter((id) => id !== requestId)
    : [...selectedRequestIds.value, requestId]
}

function clearRequestSelection() {
  selectedRequestIds.value = []
}

function providerSupportsRequest(request: ResourceRequestSchema, binding: string): boolean {
  if (!binding) return false
  if (!request.requestedResources.gpu) return true
  if (approval.providerOptions.kind !== 'success') return false
  return approval.providerOptions.data.some((option) =>
    option.providerBinding === binding
      && option.gpuClasses.includes(request.requestedResources.gpu!.class),
  )
}

const validBatchProviderBinding = computed(() => {
  const binding = batchProviderBinding.value.trim()
  const hasControlCharacter = Array.from(binding).some((character) => {
    const code = character.charCodeAt(0)
    return code < 0x20 || code === 0x7f
  })
  return binding.length > 0
    && binding.length <= 120
    && !hasControlCharacter
    && selectedBatchRequests.value.every((request) => providerSupportsRequest(request, binding))
})

const validBatchReason = computed(() => {
  const length = batchReason.value.trim().length
  return length >= 1 && length <= 500
})

const canBatchApprove = computed(() => selectedBatchRequests.value.length > 0
  && validBatchProviderBinding.value
  && validBatchReason.value)

const batchItems = computed<RequestActionItem[]>(() => selectedBatchRequests.value.map((request) => ({
  requestId: request.id,
  expectedRevision: request.revision,
  expectedFingerprint: requestFingerprint(request),
  payload: {
    providerBinding: batchProviderBinding.value.trim(),
    resources: copyResources(request.requestedResources),
    durationSeconds: request.requestedDurationSeconds,
    reason: batchReason.value.trim(),
  },
})))

const batchRequestLabels = ref(new Map<string, string>())

function batchRequestLabel(requestId: string): string {
  return batchRequestLabels.value.get(requestId) ?? requestId
}

function batchOutcomeLabel(kind: BatchActionOutcome['kind']): string {
  return { success: '全部已受理', partial: '部分已受理', error: '未有项目受理' }[kind]
}

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
      state: resourceLeaseStateLabel(lease.state),
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

const requiresGpuProvider = computed(() => {
  if (!resizeMode.value) return Boolean(approval.selectedRequest?.requestedResources.gpu)
  return resizeGpuClass.value.trim().length > 0
})

const validProviderBinding = computed(() => {
  const binding = providerBinding.value.trim()
  const hasControlCharacter = Array.from(binding).some((character) => {
    const code = character.charCodeAt(0)
    return code < 0x20 || code === 0x7f
  })
  if (!binding || binding.length > 120 || hasControlCharacter) return false
  return requiresGpuProvider.value
    ? eligibleProviderOptions.value.some((option) => option.providerBinding === binding)
    : true
})

const validApproveInputs = computed(
  () => validProviderBinding.value
    && Number.isInteger(approveDuration.value)
    && approveDuration.value > 0,
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

const eligibleProviderOptions = computed(() => {
  if (approval.providerOptions.kind !== 'success' || !approval.selectedRequest) return []
  const resources = resizeMode.value ? resizeResources() : approval.selectedRequest.requestedResources
  if (!resources.gpu) return approval.providerOptions.data
  return approval.providerOptions.data.filter((option) => option.gpuClasses.includes(resources.gpu!.class))
})

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
  [() => approval.selectedRequest, () => approval.providerOptions],
  () => {
    requestReason.value = ''
    resizeMode.value = false
    providerBinding.value = DEFAULT_PROVIDER_BINDING
    const selectedRequest = approval.selectedRequest
    if (!selectedRequest) return
    approveDuration.value = selectedRequest.requestedDurationSeconds
    resizeCpuMillicores.value = selectedRequest.requestedResources.cpuMillicores
    resizeMemoryGiB.value = Math.round(selectedRequest.requestedResources.memoryBytes / GIB)
    resizeStorageGiB.value = Math.round(selectedRequest.requestedResources.storageBytes / GIB)
    resizeGpuClass.value = selectedRequest.requestedResources.gpu?.class ?? ''
    resizeGpuCount.value = selectedRequest.requestedResources.gpu?.count ?? 0
    if (requiresGpuProvider.value) providerBinding.value = eligibleProviderOptions.value[0]?.providerBinding ?? ''
  },
  { immediate: true },
)

watch(eligibleProviderOptions, (options) => {
  if (!requiresGpuProvider.value) return
  if (!options.some((option) => option.providerBinding === providerBinding.value)) {
    providerBinding.value = options[0]?.providerBinding ?? ''
  }
})

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
const pendingRequestItem = ref<RequestActionItem | null>(null)
const pendingBatchItems = ref<RequestActionItem[]>([])
const pendingRequestDisplay = ref<{
  requestKey: string
  revision: number
  resources: WorkloadResources
} | null>(null)
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
  const item = pendingRequestItem.value
  const display = pendingRequestDisplay.value
  if (!item || !display || !pendingRequestAction.value) return ''
  const base = `将对申请 ${display.requestKey}（rev-${display.revision}）执行操作，理由：${item.payload.reason}`
  return pendingRequestAction.value === 'resize'
    ? `${base}。调整后规格：${formatResources(item.payload.resources)}。`
    : `${base}。`
})

function openRequestConfirm(kind: RequestActionKind) {
  const request = approval.selectedRequest
  if (!request) return
  const resources = kind === 'resize' ? resizeResources() : request.requestedResources
  const item: RequestActionItem = {
    requestId: request.id,
    expectedRevision: request.revision,
    expectedFingerprint: requestFingerprint(request),
    payload: {
      providerBinding: providerBinding.value.trim(),
      resources: {
        ...resources,
        ...(resources.gpu ? { gpu: { ...resources.gpu } } : {}),
      },
      durationSeconds: approveDuration.value,
      reason: requestReason.value.trim(),
    },
  }
  pendingRequestItem.value = item
  pendingRequestDisplay.value = {
    requestKey: request.requestKey,
    revision: request.revision,
    resources: item.payload.resources,
  }
  pendingRequestAction.value = kind
}

function cancelRequestConfirm() {
  pendingRequestAction.value = null
  pendingRequestItem.value = null
  pendingRequestDisplay.value = null
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
  const item = pendingRequestItem.value
  cancelRequestConfirm()
  if (!kind || !item) return
  const ok = await approval.runRequestAction(kind, item.requestId, item.payload, item)
  if (ok) {
    requestReason.value = ''
    resizeMode.value = false
  }
}

const batchConfirmDescription = computed(() => {
  const items = pendingBatchItems.value
  if (items.length === 0) return ''
  const labels = items.map((item) => `${batchRequestLabel(item.requestId)}（${item.requestId}，rev-${item.expectedRevision}）`).join('、')
  const reason = items[0]?.payload.reason ?? ''
  return `将按当前列表中明确选择的 ${items.length} 项 task 请求逐项批准：${labels}。每项会再次读取确认时的 revision 和内容；如果任一请求已变化，该项会单独失败，不会自动替换或跳过确认。理由：${reason}。`
})

function openBatchConfirm() {
  if (!canBatchApprove.value) return
  pendingBatchItems.value = batchItems.value
  batchRequestLabels.value = new Map(
    selectedBatchRequests.value.map((request) => [request.id, request.requestKey]),
  )
  pendingBatchApproval.value = true
}

function cancelBatchConfirm() {
  pendingBatchApproval.value = false
  pendingBatchItems.value = []
}

async function onBatchConfirmed() {
  pendingBatchApproval.value = false
  const items = pendingBatchItems.value
  pendingBatchItems.value = []
  if (items.length === 0) return
  const result = await approval.runRequestActions('approve', items)
  selectedRequestIds.value = result.items
    .filter((item) => item.kind === 'error')
    .map((item) => item.requestId)
  if (result.kind === 'success') batchReason.value = ''
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
  flex-wrap: wrap;
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

.filter-input {
  min-width: 220px;
  flex: 1 1 240px;
}

.filter-hint {
  margin: -4px 0 12px;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.selection-unavailable {
  color: var(--md-sys-color-on-surface-variant);
}

.batch-approval-panel {
  display: flex;
  flex-direction: column;
  gap: 12px;
  margin-top: 16px;
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.batch-approval-heading {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}

.batch-approval-heading h4 {
  margin: 0;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-medium);
}

.batch-approval-heading p,
.batch-approval-hint {
  margin: 4px 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.batch-selection-count {
  flex-shrink: 0;
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
}

.batch-approval-fields {
  display: grid;
  grid-template-columns: 180px minmax(0, 1fr);
  align-items: center;
  gap: 8px 12px;
}

.batch-approval-fields label {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
}

.batch-outcome {
  padding-top: 12px;
  border-top: 1px solid var(--md-sys-color-outline-variant);
}

.batch-outcome ul {
  display: grid;
  gap: 8px;
  margin: 8px 0 0;
  padding-left: 20px;
}

.batch-outcome-item {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: baseline;
}

.batch-outcome-item--error {
  color: var(--md-sys-color-error);
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

.provider-binding-hint {
  grid-column: 2;
  margin: 0;
  padding: 6px 10px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-tertiary-container);
  color: var(--md-sys-color-on-tertiary-container);
  font: var(--md-sys-body-small);
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

@media (max-width: 720px) {
  .batch-approval-heading,
  .batch-approval-fields {
    display: flex;
    flex-direction: column;
    align-items: stretch;
  }
}
</style>
