<template>
  <div class="data-table" role="region" :aria-label="ariaLabel ?? (attrs['aria-label'] as string | undefined)" tabindex="0">
    <table>
      <thead>
        <tr>
          <th
            v-for="column in columns"
            :key="column.key"
            :style="{ width: column.width, minWidth: column.minWidth }"
          >
            {{ column.title }}
          </th>
        </tr>
      </thead>
      <tbody>
        <tr v-if="loading">
          <td :colspan="columns.length" class="data-table__state">
            <span class="skeleton-row" v-for="i in skeletonRows" :key="i" />
          </td>
        </tr>
        <tr v-else-if="rows.length === 0">
          <td :colspan="columns.length" class="data-table__state">
            <slot name="empty">
              <div class="empty-cell">
                <SvgIcon name="info" size="lg" aria-hidden="true" />
                <span>{{ emptyText }}</span>
              </div>
            </slot>
          </td>
        </tr>
        <tr
          v-for="(row, rowIndex) in rows"
          :key="rowKey(row, rowIndex)"
          class="data-table__row"
          :class="{ 'data-table__row--interactive': interactive }"
          @click="interactive && $emit('row-click', row)"
        >
          <td v-for="column in columns" :key="column.key">
            <slot :name="column.key" :row="row" :value="row[column.key]">
              {{ formatCell(row[column.key]) }}
            </slot>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<script setup lang="ts">
import { useAttrs } from 'vue'
import SvgIcon from './SvgIcon.vue'

export interface DataTableColumn<T extends object = Record<string, unknown>> {
  key: keyof T & string
  title: string
  width?: string
  minWidth?: string
}

interface Props {
  // The table renders arbitrary domain rows. Callers keep their row-specific
  // column type while this SFC accepts all object-shaped rows in templates.
  columns: DataTableColumn<any>[]
  rows: any[]
  loading?: boolean
  emptyText?: string
  interactive?: boolean
  ariaLabel?: string
  skeletonRows?: number
}

withDefaults(defineProps<Props>(), {
  loading: false,
  emptyText: '暂无数据',
  interactive: false,
  skeletonRows: 3,
})

const attrs = useAttrs()

defineEmits<{
  (e: 'row-click', row: any): void
}>()

function rowKey(row: any, index: number): string {
  const keyedRow = row as { id?: unknown; key?: unknown }
  if (keyedRow.id !== undefined && keyedRow.id !== null) return String(keyedRow.id)
  if (keyedRow.key !== undefined && keyedRow.key !== null) return String(keyedRow.key)
  return `row-${index}`
}

function formatCell(value: unknown): string {
  if (value === null || value === undefined) return '—'
  return String(value)
}
</script>

<style scoped>
.data-table {
  overflow-x: auto;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
}

.data-table:focus-visible {
  outline: 2px solid var(--md-sys-color-primary);
  outline-offset: 2px;
}

table {
  width: 100%;
  min-width: 600px;
  border-collapse: collapse;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface);
}

th,
td {
  padding: 12px 16px;
  text-align: left;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

th {
  height: 40px;
  background: var(--md-sys-color-surface-container);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-large);
  white-space: nowrap;
}

tr:last-child td {
  border-bottom: none;
}

.data-table__row--interactive {
  cursor: pointer;
}

.data-table__row--interactive:hover {
  background: var(--md-sys-color-surface-container);
}

.data-table__state {
  padding: 0;
}

.empty-cell {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 48px 24px;
  color: var(--md-sys-color-on-surface-variant);
}

.skeleton-row {
  display: block;
  height: 40px;
  margin: 8px 16px;
  border-radius: var(--md-sys-shape-small);
  background: linear-gradient(
    90deg,
    var(--md-sys-color-surface-container) 25%,
    var(--md-sys-color-surface-container-high) 50%,
    var(--md-sys-color-surface-container) 75%
  );
  background-size: 200% 100%;
  animation: shimmer 1.5s infinite;
}

@keyframes shimmer {
  0% {
    background-position: 200% 0;
  }
  100% {
    background-position: -200% 0;
  }
}

@media (prefers-reduced-motion: reduce) {
  .skeleton-row {
    animation: none;
  }
}
</style>
