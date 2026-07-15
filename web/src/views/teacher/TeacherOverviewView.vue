<template>
  <section class="overview">
    <div class="signal-grid" aria-label="教学闭环状态">
      <article v-for="signal in signals" :key="signal.label" class="signal-card md-card">
        <span>{{ signal.label }}</span>
        <strong>{{ signal.value }}</strong>
        <small>{{ signal.note }}</small>
      </article>
    </div>
    <section class="table-section md-card">
      <header>
        <div>
          <h2>课程 / 实验</h2>
          <p>课程、环境、审批与评测 API 尚未绑定；当前不会展示业务数据。</p>
        </div>
      </header>
      <DataTable
        :columns="columns"
        :rows="[]"
        :loading="false"
        empty-text="未绑定数据源，未展示实验条目。"
        aria-label="课程实验列表"
      />
    </section>
  </section>
</template>

<script setup lang="ts">
import DataTable from '@/components/common/DataTable.vue'
import type { DataTableColumn } from '@/components/common/DataTable.vue'

const signals = [
  { label: '进行中实验', value: '—', note: '等待课程 API 绑定' },
  { label: '待审批', value: '—', note: '等待审批 API 绑定' },
  { label: '环境异常', value: '—', note: '等待环境事件绑定' },
  { label: '最近评测', value: '—', note: '等待评测 API 绑定' },
]

const columns: DataTableColumn[] = [
  { key: 'name', title: '实验' },
  { key: 'status', title: '状态' },
  { key: 'approvals', title: '待审批' },
  { key: 'environment', title: '环境' },
  { key: 'evaluation', title: '最近评测' },
]
</script>

<style scoped>
.overview {
  display: grid;
  gap: 20px;
}

.signal-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 16px;
}

.signal-card {
  display: grid;
  gap: 7px;
  padding: 18px;
}

.signal-card span,
.signal-card small {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.signal-card strong {
  font: var(--md-sys-headline-small);
  color: var(--md-sys-color-on-surface);
}

.table-section {
  overflow: hidden;
}

.table-section header {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  padding: 20px 20px 16px;
}

.table-section h2 {
  font: var(--md-sys-title-large);
  color: var(--md-sys-color-on-surface);
}

.table-section p {
  margin-top: 4px;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

@media (max-width: 760px) {
  .signal-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 599px) {
  .signal-grid {
    grid-template-columns: 1fr;
  }

  .table-section header {
    display: block;
  }
}
</style>
