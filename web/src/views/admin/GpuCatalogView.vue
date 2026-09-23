<template>
  <div class="gpu-catalog-page">
    <header class="page-header">
      <div>
        <h2>GPU 目录</h2>
        <p class="page-subtitle">维护 GPU class 目录：Resource 按目录解析申请使用的分配模式与容量单位，provider binding 决定由哪个容量提供者观察可用量。</p>
      </div>
      <button type="button" class="icon-button" aria-label="刷新 GPU 目录" :disabled="busy" @click="catalog.load">
        <SvgIcon name="refresh" size="sm" aria-hidden="true" />
      </button>
    </header>

    <DiagnosticBanner
      v-if="bannerFailure"
      :code="bannerFailure.code"
      :message="bannerFailure.message"
      :retryable="bannerFailure.retryable"
      severity="error"
      @retry="catalog.load"
    />

    <section class="catalog-card md-card" aria-labelledby="gpu-catalog-heading">
      <div class="section-heading">
        <div>
          <h3 id="gpu-catalog-heading">目录</h3>
          <p>目录保留历史 revision；同一 class 只有最新且「可用」的条目参与分配。</p>
        </div>
      </div>

      <AsyncStateView :state="catalogState" empty-text="GPU 目录为空。" @retry="catalog.load">
        <template #success="{ data }">
          <!-- @vue-generic {CatalogRow} -->
          <DataTable
            :columns="catalogColumns"
            :rows="data"
            class="catalog-table"
            aria-label="GPU 目录"
          >
            <template #class="{ row }"><code>{{ row.class }}</code></template>
            <template #mode="{ row }">{{ modeLabel(row.mode) }}</template>
            <template #providerBinding="{ row }"><code>{{ row.providerBinding }}</code></template>
            <template #allocationBinding="{ row }"><code>{{ row.allocationBinding }}</code></template>
            <template #active="{ row }">
              <span class="state-chip" :class="row.active ? 'state-chip--active' : 'state-chip--disabled'">{{ row.active ? '可用' : '已停用' }}</span>
            </template>
          </DataTable>
        </template>
      </AsyncStateView>
    </section>

    <section class="create-card md-card" aria-labelledby="gpu-create-heading">
      <div class="section-heading">
        <div>
          <h3 id="gpu-create-heading">新建目录项</h3>
          <p>新建条目会以声明的 revision 取代同 class 的当前条目；revision 必须高于该 class 已有的最大版本。</p>
        </div>
      </div>
      <form class="admin-form" @submit.prevent="submitCreate">
        <label>
          <span>class</span>
          <input v-model="createForm.class" class="text-input" maxlength="63" pattern="[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?" required />
        </label>
        <label>
          <span>mode</span>
          <select v-model="createForm.mode" class="text-input">
            <option value="exclusive">exclusive</option>
            <option value="container_time_slice">container_time_slice</option>
            <option value="vm_vgpu">vm_vgpu</option>
          </select>
        </label>
        <label>
          <span>provider binding</span>
          <input v-model="createForm.providerBinding" class="text-input" maxlength="120" required />
        </label>
        <label>
          <span>capacity units</span>
          <input v-model.number="createForm.capacityUnits" class="text-input" type="number" min="1" step="1" required />
        </label>
        <label>
          <span>allocation binding</span>
          <input v-model="createForm.allocationBinding" class="text-input" maxlength="256" required />
        </label>
        <label>
          <span>revision</span>
          <input v-model.number="createForm.revision" class="text-input" type="number" min="1" step="1" required />
        </label>
        <button type="submit" class="filled-button" :disabled="busy">创建</button>
      </form>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive } from 'vue'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DataTable, { type DataTableColumn } from '@/components/common/DataTable.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useGpuCatalog } from '@/composables/useGpuCatalog'
import type { AsyncState, DiagnosticViewModel } from '@/types/async'
import type {
  GpuAllocationMode,
  GpuCatalogEntrySchema,
} from '@/generated/contracts'

type CatalogRow = GpuCatalogEntrySchema & { actions?: never }

const catalog = useGpuCatalog()

const createForm = reactive<{
  class: string
  mode: GpuAllocationMode
  providerBinding: string
  capacityUnits: number
  allocationBinding: string
  revision: number
}>({ class: '', mode: 'exclusive', providerBinding: '', capacityUnits: 1, allocationBinding: '', revision: 1 })

const catalogColumns: DataTableColumn<CatalogRow>[] = [
  { key: 'class', title: 'class' },
  { key: 'mode', title: 'mode' },
  { key: 'providerBinding', title: 'provider binding' },
  { key: 'allocationBinding', title: 'allocation binding' },
  { key: 'capacityUnits', title: 'capacity units' },
  { key: 'revision', title: '版本' },
  { key: 'active', title: '状态' },
]

const busy = computed(() => catalog.state.kind === 'loading' || catalog.state.kind === 'submitting')

/**
 * The catalog keeps rendering the last server projection while a write is in
 * flight or has failed; only a failed first load replaces the table.
 */
const catalogState = computed<AsyncState<GpuCatalogEntrySchema[]>>(() => {
  if (catalog.state.kind === 'error' && catalog.entries.length === 0) {
    return { kind: 'error', diagnostic: catalog.state.diagnostic }
  }
  if (catalog.state.kind === 'idle') return { kind: 'idle' }
  if (catalog.state.kind === 'loading') return { kind: 'loading', message: '加载 GPU 目录…' }
  return catalog.entries.length > 0 ? { kind: 'success', data: catalog.entries } : { kind: 'empty' }
})

const bannerFailure = computed<DiagnosticViewModel | null>(() => {
  if (catalog.state.kind !== 'error') return null
  return catalog.entries.length > 0 ? catalog.state.diagnostic : null
})

function modeLabel(mode: GpuAllocationMode): string {
  return ({ exclusive: '独占', container_time_slice: '容器时间片', vm_vgpu: 'VM vGPU' } as Record<GpuAllocationMode, string>)[mode]
}

async function submitCreate() {
  const created = await catalog.create({ ...createForm })
  if (!created) return
  createForm.class = ''
  createForm.providerBinding = ''
  createForm.allocationBinding = ''
}

onMounted(() => catalog.load())
</script>

<style scoped>
.gpu-catalog-page { display: grid; gap: 20px; }
.page-header, .section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.page-header h2, .section-heading h3 { margin: 0; color: var(--md-sys-color-on-surface); }
.page-header h2 { font: var(--md-sys-headline-small); }
.section-heading h3 { font: var(--md-sys-title-large); }
.page-subtitle, .section-heading p { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.catalog-card, .create-card { display: grid; gap: 16px; padding: 20px; }
.admin-form { display: grid; gap: 12px; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); align-items: end; }
.admin-form label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.admin-form button { justify-self: start; }
.text-input { box-sizing: border-box; min-height: 40px; width: 100%; padding: 8px 11px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.state-chip { display: inline-flex; white-space: nowrap; padding: 3px 8px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.state-chip--active { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--disabled { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
.filled-button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 40px; padding: 0 17px; border: 1px solid var(--md-sys-color-primary); border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); font: var(--md-sys-label-large); cursor: pointer; }
.filled-button:disabled { opacity: .5; cursor: not-allowed; }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
.icon-button:disabled { opacity: .5; cursor: not-allowed; }
@media (max-width: 620px) { .admin-form { grid-template-columns: 1fr; } }
</style>
