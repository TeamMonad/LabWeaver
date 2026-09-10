<template>
  <div class="resource-page">
    <header class="page-header">
      <div>
        <h2>资源申请</h2>
        <p class="page-subtitle">申请会归属于所选项目。审批、分配和回收会在后台完成。</p>
      </div>
      <RouterLink class="outlined-button" to="/researcher/workspaces">选择项目</RouterLink>
    </header>

    <section class="project-strip md-card">
      <label>
        <span>项目</span>
        <select v-model="selectedProjectId" class="text-input" :disabled="projects.projects.kind !== 'success'">
          <option v-for="project in projectOptions" :key="project.id" :value="project.id">{{ project.name }} · {{ project.id }}</option>
        </select>
      </label>
      <span v-if="selectedProject" class="project-scope">{{ selectedProject.courseId ? `课程 ${selectedProject.courseId}` : '独立科研项目' }}</span>
    </section>

    <DiagnosticBanner
      v-if="resources.outcome"
      :code="resources.outcome.diagnostic.code"
      :message="resources.outcome.diagnostic.message"
      :retryable="resources.outcome.diagnostic.retryable"
      :severity="resources.outcome.kind === 'error' ? 'error' : 'info'"
      @retry="resources.load"
    />

    <DiagnosticBanner
      v-if="options.environments.kind === 'error'"
      :code="options.environments.diagnostic.code"
      :message="options.environments.diagnostic.message"
      :retryable="options.environments.diagnostic.retryable"
      severity="error"
      @retry="options.load"
    />
    <DiagnosticBanner
      v-if="options.releases.kind === 'error'"
      :code="options.releases.diagnostic.code"
      :message="options.releases.diagnostic.message"
      :retryable="options.releases.diagnostic.retryable"
      severity="error"
      @retry="options.load"
    />
    <DiagnosticBanner
      v-if="options.catalog.kind === 'error' || options.rates.kind === 'error'"
      :code="options.catalog.kind === 'error' ? options.catalog.diagnostic.code : options.rates.kind === 'error' ? options.rates.diagnostic.code : ''"
      :message="options.catalog.kind === 'error' ? options.catalog.diagnostic.message : options.rates.kind === 'error' ? options.rates.diagnostic.message : ''"
      :retryable="options.catalog.kind === 'error' ? options.catalog.diagnostic.retryable : options.rates.kind === 'error' ? options.rates.diagnostic.retryable : false"
      severity="error"
      @retry="options.load"
    />

    <div class="resource-layout">
      <section class="request-form-card md-card" aria-labelledby="request-heading">
        <h3 id="request-heading">申请 Work 容量</h3>
        <p class="section-note">可用 GPU 类型与分配方式由平台配置，申请时只能选择已配置的目录项。</p>
        <form class="request-form" @submit.prevent="submitRequest">
          <div class="generated-identity" role="note">
            <strong>新建 Work</strong>
            <span>系统会为本次申请生成唯一工作环境，并在审批通过后创建环境。</span>
          </div>
          <label>
            <span>已发布版本</span>
            <select v-model="selectedReleaseKey" class="text-input" required :disabled="options.releases.kind !== 'success'">
              <option value="">选择用于绑定的版本</option>
              <option v-for="release in releaseOptions" :key="releaseKey(release.id, release.version)" :value="releaseKey(release.id, release.version)">
                {{ release.label }} · {{ release.runtimeKind }}
              </option>
            </select>
            <small v-if="options.releases.kind === 'empty'" class="field-note">没有可用的已发布版本。</small>
          </label>
          <div class="two-columns">
            <label><span>CPU（millicores）</span><input v-model.number="cpuMillicores" class="text-input" type="number" min="1" required /></label>
            <label><span>时长（小时）</span><input v-model.number="durationHours" class="text-input" type="number" min="1" max="720" required /></label>
          </div>
          <div class="two-columns">
            <label><span>内存（GiB）</span><input v-model.number="memoryGiB" class="text-input" type="number" min="1" required /></label>
            <label><span>存储（GiB）</span><input v-model.number="storageGiB" class="text-input" type="number" min="1" required /></label>
          </div>
          <div class="two-columns">
            <label>
              <span>GPU 目录项（可选）</span>
              <select v-model="selectedGpuCatalogId" class="text-input" :disabled="options.catalog.kind !== 'success'">
                <option value="">不申请 GPU</option>
                <option v-for="entry in gpuCatalogOptions" :key="entry.id" :value="entry.id">
                  {{ entry.class }} · {{ gpuModeLabel(entry.mode) }} · {{ entry.capacityUnits }} units
                </option>
              </select>
            </label>
            <label>
              <span>GPU 数量</span>
              <input v-model.number="gpuCount" class="text-input" type="number" min="1" :max="selectedGpu?.capacityUnits ?? 1" :disabled="!selectedGpu || selectedGpu.mode === 'container_time_slice'" />
            </label>
          </div>
          <div v-if="selectedGpu" class="gpu-detail" role="note">
            <strong>{{ selectedGpu.class }} · {{ gpuModeLabel(selectedGpu.mode) }}</strong>
            <span>目录容量：{{ selectedGpu.capacityUnits }} units · 单次申请上限：{{ selectedGpu.mode === 'container_time_slice' ? '1 个共享时间片' : `${selectedGpu.capacityUnits} 个单位` }}</span>
            <span v-if="selectedGpuRate">费率：{{ selectedGpuRate.unitPrice.amount }} {{ selectedGpuRate.unitPrice.currency }} / {{ selectedGpuRate.unitQuantity }} GPU 秒</span>
            <span v-else-if="selectedGpuRateAmbiguous">费率：当前有效费率的同一版本存在冲突，无法提交</span>
            <span v-else>费用：尚未配置，当前不能提交这项 GPU 申请。</span>
          </div>
          <div class="estimate-box" role="note">
            <span>提交规格</span>
            <strong>{{ requestSummary }}</strong>
            <small>费用估算以当前配置为准；没有匹配费率时会显示未配置计价。</small>
          </div>
          <button type="submit" class="filled-button" :disabled="!canSubmit || resources.acting !== null">提交资源申请</button>
        </form>
      </section>

      <section class="status-card md-card" aria-labelledby="status-heading">
        <div class="section-heading">
          <div>
            <h3 id="status-heading">申请与 Lease</h3>
            <p>刷新不会重复创建请求；异步状态会在页面可见时自动更新。</p>
          </div>
          <button type="button" class="icon-button" aria-label="刷新资源状态" :disabled="resources.requests.kind === 'loading'" @click="resources.load">
            <SvgIcon name="refresh" size="sm" aria-hidden="true" />
          </button>
        </div>

        <section class="status-section" aria-labelledby="requests-heading">
          <h4 id="requests-heading">资源申请</h4>
          <AsyncStateView :state="resources.requests" empty-text="该项目暂无资源申请。" @retry="resources.load">
            <template #success="{ data }">
              <ul class="resource-list">
                <li v-for="request in data" :key="request.id" class="resource-row">
                  <div class="resource-row__main"><strong>{{ request.requestKey }}</strong><small>{{ resourceTargetLabel(request) }} · {{ resourceSummary(request.requestedResources) }}</small><small>更新于 {{ formatTimestamp(request.updatedAt) }}</small></div>
                  <div class="resource-row__actions"><span class="state-chip" :class="`state-chip--${request.state}`">{{ requestStateLabel(request.state) }}</span><button v-if="request.state === 'reviewing' || request.state === 'allocating'" type="button" class="text-button danger-button" :disabled="resources.acting !== null" @click="cancelRequest(request.id)">取消</button></div>
                </li>
              </ul>
            </template>
          </AsyncStateView>
        </section>

        <section class="status-section" aria-labelledby="leases-heading">
          <h4 id="leases-heading">资源 Lease</h4>
          <AsyncStateView :state="resources.leases" empty-text="该项目暂无 Lease。" @retry="resources.load">
            <template #success="{ data }">
              <ul class="resource-list">
                <li v-for="lease in data" :key="lease.id" class="resource-row">
                  <div class="resource-row__main"><strong>{{ lease.id }}</strong><small>申请 {{ lease.requestId }} · {{ lease.expiresAt ? `到期 ${formatTimestamp(lease.expiresAt)}` : '等待分配' }}</small><small v-if="lease.revokeReasonCode">原因：{{ lease.revokeReasonCode }}</small></div>
                  <div class="resource-row__actions"><span class="state-chip" :class="`state-chip--${lease.state}`">{{ leaseStateLabel(lease.state) }}</span><button v-if="lease.state === 'active' || lease.state === 'expiring'" type="button" class="outlined-button small" :disabled="resources.acting !== null" @click="renewLease(lease)">续期</button><button v-if="lease.state === 'active' || lease.state === 'expiring'" type="button" class="text-button danger-button" :disabled="resources.acting !== null" @click="reclaimLease(lease)">回收</button><RouterLink v-if="lease.state === 'active' && requestEnvironmentId(lease.requestId)" class="text-button" :to="{ path: '/researcher/environments', query: { environmentId: requestEnvironmentId(lease.requestId)!, projectId: selectedProjectId ?? undefined } }">连接</RouterLink></div>
                </li>
              </ul>
            </template>
          </AsyncStateView>
        </section>
      </section>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useProjectResources, resourceSummary } from '@/composables/useProjectResources'
import { useProjectResourceOptions, type ResourceGpuCatalogOption } from '@/composables/useProjectResourceOptions'
import { useProjects } from '@/composables/useProjects'
import type { ResourceLeaseSchema, ResourceRequestSchema } from '@/generated/contracts'
import { formatTimestamp, idempotencyKey, newUuidV7 } from '@/utils/format'

const route = useRoute()
const router = useRouter()
const projects = useProjects()
const selectedProjectId = ref<string | null>(null)
const projectIdRef = computed(() => selectedProjectId.value)
const projectOptions = computed(() => projects.projects.kind === 'success' ? projects.projects.data : [])
const selectedProject = computed(() => projectOptions.value.find((project) => project.id === selectedProjectId.value) ?? null)
const routeProjectId = computed(() => {
  const id = typeof route.query.projectId === 'string' ? route.query.projectId.trim() : ''
  return id || null
})
const resources = useProjectResources(projectIdRef)
const courseIdRef = computed(() => selectedProject.value?.courseId ?? null)
const options = useProjectResourceOptions(projectIdRef, courseIdRef)

const selectedReleaseKey = ref('')
const cpuMillicores = ref(2000)
const memoryGiB = ref(4)
const storageGiB = ref(20)
const durationHours = ref(8)
const selectedGpuCatalogId = ref('')
const gpuCount = ref(1)
const pendingSubmission = ref<{ environmentId: string; idempotencyKey: string } | null>(null)

const releaseOptions = computed(() => options.releases.kind === 'success' ? options.releases.data : [])
const gpuCatalogOptions = computed(() => options.catalog.kind === 'success' ? options.catalog.data : [])
const selectedRelease = computed(() => releaseOptions.value.find((item) => releaseKey(item.id, item.version) === selectedReleaseKey.value) ?? null)
const selectedGpu = computed(() => gpuCatalogOptions.value.find((item) => item.id === selectedGpuCatalogId.value) ?? null)
const selectedGpuRateSelection = computed(() => selectedGpu.value
  ? options.gpuRateSelection(selectedGpu.value)
  : { rate: null, ambiguous: false })
const selectedGpuRate = computed(() => selectedGpuRateSelection.value.rate)
const selectedGpuRateAmbiguous = computed(() => selectedGpuRateSelection.value.ambiguous)

watch(
  () => projectOptions.value,
  (items) => {
    const preferred = routeProjectId.value ?? projects.selectedProjectId
    const next = preferred && items.some((project) => project.id === preferred) ? preferred : items[0]?.id ?? null
    if (selectedProjectId.value !== next) selectedProjectId.value = next
    if (next && projects.selectedProjectId !== next) projects.select(next)
  },
  { immediate: true },
)

watch(routeProjectId, (id) => {
  if (!id || !projectOptions.value.some((project) => project.id === id)) return
  if (selectedProjectId.value !== id) selectedProjectId.value = id
  if (projects.selectedProjectId !== id) projects.select(id)
})

watch(selectedProjectId, (id) => {
  if (!id || route.query.projectId === id) return
  void router.replace({ query: { ...route.query, projectId: id } })
})

watch(
  () => releaseOptions.value,
  (items) => {
    if (!items.some((item) => releaseKey(item.id, item.version) === selectedReleaseKey.value)) {
      selectedReleaseKey.value = releaseKey(items[0]?.id ?? '', items[0]?.version ?? 0)
    }
  },
  { immediate: true },
)

watch(
  () => selectedGpu.value,
  (entry) => {
    if (!entry) {
      gpuCount.value = 1
    } else if (entry.mode === 'container_time_slice') {
      gpuCount.value = 1
    } else if (gpuCount.value < 1 || gpuCount.value > entry.capacityUnits) {
      gpuCount.value = Math.min(Math.max(gpuCount.value, 1), entry.capacityUnits)
    }
  },
)

const canSubmit = computed(() => Boolean(
  selectedProjectId.value &&
  selectedRelease.value &&
  Number.isInteger(cpuMillicores.value) && cpuMillicores.value > 0 &&
  Number.isInteger(memoryGiB.value) && memoryGiB.value > 0 &&
  Number.isInteger(storageGiB.value) && storageGiB.value > 0 &&
  Number.isInteger(durationHours.value) && durationHours.value > 0 &&
  (!selectedGpu.value || (
    Number.isInteger(gpuCount.value) &&
    gpuCount.value > 0 &&
    gpuCount.value <= selectedGpu.value.capacityUnits
  ))
))

watch(
  [selectedProjectId, selectedReleaseKey, cpuMillicores, memoryGiB, storageGiB, durationHours, selectedGpuCatalogId, gpuCount],
  () => {
    // A changed request body must receive a new aggregate identity and key. A
    // failed retry with the same body keeps both values so the server can
    // replay the idempotent command without creating another Work.
    pendingSubmission.value = null
  },
)
const requestSummary = computed(() => resourceSummary({
  cpuMillicores: cpuMillicores.value,
  memoryBytes: memoryGiB.value * 1024 ** 3,
  storageBytes: storageGiB.value * 1024 ** 3,
  ...(selectedGpu.value ? { gpu: { class: selectedGpu.value.class, count: gpuCount.value } } : {}),
}))

async function submitRequest() {
  if (!canSubmit.value) return
  const submission = pendingSubmission.value ?? {
    environmentId: newUuidV7(),
    idempotencyKey: idempotencyKey(),
  }
  pendingSubmission.value = submission
  const ok = await resources.create({
    courseId: selectedProject.value?.courseId ?? undefined,
    durationSeconds: durationHours.value * 3600,
    requestKey: `work-${submission.environmentId}`,
    resources: { cpuMillicores: cpuMillicores.value, memoryBytes: memoryGiB.value * 1024 ** 3, storageBytes: storageGiB.value * 1024 ** 3, ...(selectedGpu.value ? { gpu: { class: selectedGpu.value.class, count: gpuCount.value } } : {}) },
    target: { kind: 'environment', environmentId: submission.environmentId, releaseId: selectedRelease.value!.id, releaseVersion: selectedRelease.value!.version },
  }, { idempotencyKey: submission.idempotencyKey })
  if (ok) {
    pendingSubmission.value = null
    selectedReleaseKey.value = ''
  }
}

async function cancelRequest(id: string) { await resources.cancel(id, 'researcher cancelled the pending resource request') }
async function renewLease(lease: ResourceLeaseSchema) { await resources.renew(lease, durationHours.value * 3600, 'researcher renewed the Work lease') }
async function reclaimLease(lease: ResourceLeaseSchema) { await resources.reclaim(lease, 'researcher requested Work resource reclaim') }

function requestEnvironmentId(requestId: string) {
  if (resources.requests.kind !== 'success') return null
  const request = resources.requests.data.find((item) => item.id === requestId)
  return request?.target.kind === 'environment' ? request.target.environmentId : null
}
function releaseKey(id: string, version: number) { return id && version > 0 ? `${id}:${version}` : '' }
function gpuModeLabel(mode: ResourceGpuCatalogOption['mode']) {
  return ({ exclusive: '独占', container_time_slice: '容器时间片', vm_vgpu: 'VM vGPU' } as Record<ResourceGpuCatalogOption['mode'], string>)[mode]
}
function resourceTargetLabel(request: ResourceRequestSchema) { return request.target.kind === 'environment' ? `环境 ${request.target.environmentId}` : `任务 ${request.target.taskRunId}` }
function requestStateLabel(state: string) { return ({ reviewing: '待审批', allocating: '分配中', active: '已激活', expiring: '即将到期', expired: '已到期', rejected: '已拒绝', cancelled: '已取消' } as Record<string, string>)[state] ?? state }
function leaseStateLabel(state: string) { return ({ allocating: '分配中', active: '有效', expiring: '即将到期', expired: '已到期', revoked: '已回收' } as Record<string, string>)[state] ?? state }
</script>

<style scoped>
.resource-page { display: grid; gap: 20px; }
.page-header, .section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.page-header h2, .section-heading h3, .status-section h4, .request-form-card h3 { margin: 0; color: var(--md-sys-color-on-surface); }
.page-header h2 { font: var(--md-sys-headline-small); }
.section-heading h3, .request-form-card h3 { font: var(--md-sys-title-large); }
.status-section h4 { font: var(--md-sys-title-medium); }
.page-subtitle, .section-heading p, .section-note { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.project-strip { display: flex; align-items: end; gap: 16px; padding: 16px 20px; }
.project-strip label, .request-form label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.field-note { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); line-height: 1.4; }
.project-strip label { flex: 1; max-width: 560px; }
.project-scope { padding-bottom: 10px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.generated-identity { display: grid; gap: 4px; padding: 12px 14px; border: 1px dashed var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.generated-identity strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-label-large); }
.resource-layout { display: grid; grid-template-columns: minmax(300px, .8fr) minmax(0, 1.3fr); gap: 20px; align-items: start; }
.request-form-card, .status-card { padding: 20px; }
.request-form { display: grid; gap: 14px; margin-top: 20px; }
.two-columns { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.estimate-box { display: grid; gap: 5px; padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.estimate-box strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.gpu-detail { display: grid; gap: 4px; padding: 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); font: var(--md-sys-body-small); }
.filled-button, .outlined-button, .text-button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 40px; padding: 0 17px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-large); cursor: pointer; text-decoration: none; }
.filled-button { border: 1px solid var(--md-sys-color-primary); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); }
.outlined-button { border: 1px solid var(--md-sys-color-outline); background: transparent; color: var(--md-sys-color-primary); }
.outlined-button.small { min-height: 32px; padding: 0 10px; font: var(--md-sys-label-medium); }
.text-button { min-height: 32px; padding: 0 8px; border: 0; background: transparent; color: var(--md-sys-color-primary); }
.danger-button { color: var(--md-sys-color-error); }
.filled-button:disabled, .outlined-button:disabled, .text-button:disabled { opacity: .5; cursor: not-allowed; }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
.status-card { display: grid; gap: 18px; }
.status-section { display: grid; gap: 10px; padding-top: 18px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.resource-list { display: grid; gap: 7px; margin: 0; padding: 0; list-style: none; }
.resource-row { display: flex; align-items: center; justify-content: space-between; gap: 14px; padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.resource-row__main { display: grid; gap: 4px; min-width: 0; }
.resource-row__main strong { overflow-wrap: anywhere; }
.resource-row__main small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); overflow-wrap: anywhere; }
.resource-row__actions { display: flex; align-items: center; justify-content: flex-end; gap: 4px; flex-wrap: wrap; }
.state-chip { display: inline-flex; white-space: nowrap; padding: 3px 8px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.state-chip--active { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--reviewing, .state-chip--allocating, .state-chip--expiring { background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container); }
.state-chip--rejected, .state-chip--cancelled, .state-chip--expired, .state-chip--revoked { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
@media (max-width: 850px) { .resource-layout { grid-template-columns: 1fr; } .project-strip { align-items: stretch; flex-direction: column; } .project-strip label { max-width: none; } .project-scope { padding-bottom: 0; } }
@media (max-width: 620px) { .two-columns { grid-template-columns: 1fr; } .resource-row { align-items: flex-start; flex-direction: column; } .resource-row__actions { justify-content: flex-start; } }
</style>
