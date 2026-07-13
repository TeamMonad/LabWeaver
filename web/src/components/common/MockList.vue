<template>
  <div class="mock-list">
    <div class="mock-list-header">
      <h3 class="mock-list-title">{{ title }}</h3>
      <MockBadge />
    </div>
    <p v-if="description" class="mock-list-desc">{{ description }}</p>
    <div class="mock-table-wrapper">
      <table class="mock-table">
        <thead>
          <tr>
            <th v-for="col in columns" :key="col.key">{{ col.label }}</th>
            <th>状态</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="(row, idx) in rows" :key="idx">
            <td v-for="col in columns" :key="col.key">{{ row[col.key] }}</td>
            <td>
              <span class="mock-status" :class="`mock-status--${row.statusType ?? 'neutral'}`">
                {{ row.status }}
              </span>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>

<script setup lang="ts">
import MockBadge from './MockBadge.vue'

interface Column {
  key: string
  label: string
}

interface Row {
  [key: string]: string | number | undefined
  status: string
  statusType?: 'success' | 'warning' | 'error' | 'neutral'
}

interface Props {
  title: string
  description?: string
  columns: Column[]
  rows: Row[]
}

defineProps<Props>()
</script>

<style scoped>
.mock-list {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.mock-list-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.mock-list-title {
  font: var(--md-sys-title-medium);
  color: var(--md-sys-color-on-surface);
}

.mock-list-desc {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.mock-table-wrapper {
  overflow-x: auto;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
}

.mock-table {
  width: 100%;
  border-collapse: collapse;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface);
}

.mock-table th,
.mock-table td {
  padding: 14px 16px;
  text-align: left;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.mock-table th {
  background: var(--md-sys-color-surface-variant);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-large);
}

.mock-table tr:last-child td {
  border-bottom: none;
}

.mock-status {
  display: inline-flex;
  align-items: center;
  height: 24px;
  padding: 0 10px;
  border-radius: var(--md-sys-shape-small);
  font: var(--md-sys-label-medium);
}

.mock-status--neutral {
  background: var(--md-sys-color-surface-variant);
  color: var(--md-sys-color-on-surface-variant);
}

.mock-status--success {
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
}

.mock-status--warning {
  background: #fff4e5;
  color: #8a6d1f;
}

.mock-status--error {
  background: #fce8e6;
  color: #9f2b22;
}
</style>
