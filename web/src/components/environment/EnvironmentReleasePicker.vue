<template>
  <section
    v-if="!isEnvironmentLoaded || environmentToolsOpen"
    aria-labelledby="releases-heading"
  >
    <h3
      id="releases-heading"
      class="section-title"
    >
      <SvgIcon
        name="deployed_code"
        size="sm"
        aria-hidden="true"
      />
      已发布环境模板
    </h3>
    <AsyncStateView
      v-if="!isContextMissing"
      :state="releases.releases"
      @retry="releases.load"
    >
      <template #success="{ data }">
        <DataTable
          :columns="releaseColumns"
          :rows="data"
          aria-label="已发布环境版本"
        >
          <template #runtimeKind="{ row }">
            {{ row.runtimeKind === 'container' ? '容器' : '虚拟机' }}
          </template>
          <template #publishedAt="{ row }">
            {{ formatTimestamp(row.publishedAt) }}
          </template>
          <template #actions="{ row }">
            <button
              type="button"
              class="filled-button small"
              :disabled="lifecycle.operating.has(`create:${row.id}`)"
              @click="createFromRelease(row)"
            >
              创建环境
            </button>
          </template>
        </DataTable>
      </template>
    </AsyncStateView>

    <div
      v-if="createDiagnostic"
      class="create-result"
    >
      <DiagnosticBanner
        :code="createDiagnostic.code"
        :request-id="createDiagnostic.requestId"
        :next-step="createDiagnostic.nextStep"
        :message="createDiagnostic.message"
        :retryable="createDiagnostic.retryable"
        severity="info"
        @retry="retryCreate"
      />
    </div>
    <div
      v-else-if="lifecycle.lastAccepted"
      class="create-result create-result--ok"
    >
      <SvgIcon
        name="check_circle"
        size="md"
        aria-hidden="true"
      />
      <span>
        创建请求已接受：{{ lifecycle.lastAccepted.operationId }}
        <span
          v-if="!selectedEnvironmentId"
          class="create-hint"
        >
          （正在加载新建环境…）
        </span>
      </span>
    </div>
  </section>
</template>

<script setup lang="ts">
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DataTable from '@/components/common/DataTable.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import type { DataTableColumn } from '@/components/common/DataTable.vue'
import type { EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'
import { useEnvironmentEntryContext } from '@/composables/useEnvironmentEntry'
import { formatTimestamp } from '@/utils/format'

/** 已发布模板列表：从一个版本创建环境，并显示创建结果。 */
const {
  selectedEnvironmentId,
  environmentToolsOpen,
  createDiagnostic,
  isEnvironmentLoaded,
  isContextMissing,
  releases,
  lifecycle,
  createFromRelease,
  retryCreate,
} = useEnvironmentEntryContext()

const releaseColumns: DataTableColumn<EnvironmentTemplateReleaseViewSchema & { actions?: never }>[] = [
  { key: 'runtimeKind', title: '运行方式' },
  { key: 'id', title: '版本 ID' },
  { key: 'publishedBy', title: '发布者' },
  { key: 'publishedAt', title: '发布时间' },
  { key: 'actions', title: '操作' },
]
</script>

<style scoped>
.section-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font: var(--md-sys-title-medium);
  color: var(--md-sys-color-on-surface);
  margin: 0 0 12px;
}

.create-result {
  margin-top: 12px;
  padding: 10px 12px;
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-tertiary-container);
  color: var(--md-sys-color-on-tertiary-container);
  font: var(--md-sys-body-medium);
}

.create-result--ok {
  display: flex;
  align-items: center;
  gap: 8px;
}

.create-hint {
  opacity: 0.8;
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

.filled-button.small {
  height: 32px;
  padding: 0 16px;
}

.filled-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
