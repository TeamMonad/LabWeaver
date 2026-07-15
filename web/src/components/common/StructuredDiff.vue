<template>
  <div class="structured-diff" role="table" :aria-label="ariaLabel">
    <div class="structured-diff__header" role="row">
      <span role="columnheader">字段</span>
      <span role="columnheader">变更前</span>
      <span role="columnheader">变更后</span>
    </div>
    <div
      v-for="(change, index) in changes"
      :key="index"
      class="structured-diff__row"
      :class="`structured-diff__row--${change.kind}`"
      role="row"
    >
      <span class="structured-diff__field" role="cell">{{ change.field }}</span>
      <span class="structured-diff__before" role="cell">
        <slot name="before" :change="change">
          {{ change.before ?? '—' }}
        </slot>
      </span>
      <span class="structured-diff__after" role="cell">
        <slot name="after" :change="change">
          {{ change.after ?? '—' }}
        </slot>
      </span>
    </div>
  </div>
</template>

<script setup lang="ts">
export interface DiffChange {
  field: string
  before?: string
  after?: string
  kind: 'added' | 'removed' | 'modified' | 'unchanged'
}

interface Props {
  changes: DiffChange[]
  ariaLabel?: string
}

withDefaults(defineProps<Props>(), {
  ariaLabel: '结构化差异对比',
})
</script>

<style scoped>
.structured-diff {
  display: grid;
  grid-template-columns: 1fr 1fr 1fr;
  gap: 1px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  overflow: hidden;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface);
  background: var(--md-sys-color-outline-variant);
}

.structured-diff__header,
.structured-diff__row {
  display: contents;
}

.structured-diff__header span,
.structured-diff__row span {
  padding: 12px 16px;
  background: var(--md-sys-color-surface);
}

.structured-diff__header span {
  background: var(--md-sys-color-surface-container);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-large);
}

.structured-diff__field {
  font: var(--md-sys-label-large);
}

.structured-diff__row--added .structured-diff__after {
  background: var(--md-sys-color-success-container);
}

.structured-diff__row--removed .structured-diff__before {
  background: var(--md-sys-color-error-container);
}

.structured-diff__row--modified .structured-diff__after {
  background: var(--md-sys-color-primary-container);
}

@media (max-width: 599px) {
  .structured-diff {
    grid-template-columns: 1fr;
  }

  .structured-diff__header {
    display: none;
  }

  .structured-diff__row {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 8px;
    padding: 12px;
    background: var(--md-sys-color-surface);
    border-bottom: 1px solid var(--md-sys-color-outline-variant);
  }

  .structured-diff__row span {
    padding: 4px 0;
  }

  .structured-diff__field {
    grid-column: 1 / -1;
    font: var(--md-sys-title-small);
  }

  .structured-diff__before::before {
    content: '前：';
    color: var(--md-sys-color-on-surface-variant);
  }

  .structured-diff__after::before {
    content: '后：';
    color: var(--md-sys-color-on-surface-variant);
  }
}
</style>
