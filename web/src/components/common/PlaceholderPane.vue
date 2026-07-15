<template>
  <div class="placeholder-pane">
    <SvgIcon :name="icon" class="placeholder-icon" size="xl" aria-hidden="true" />
    <h3 class="placeholder-title">{{ title }}</h3>
    <p class="placeholder-desc">{{ description }}</p>
    <div v-if="showHint" class="placeholder-hint">
      <SvgIcon name="info" size="sm" aria-hidden="true" />
      <span>{{ hint }}</span>
    </div>
  </div>
</template>

<script setup lang="ts">
import SvgIcon from './SvgIcon.vue'

interface Props {
  icon: string
  title: string
  description: string
  showHint?: boolean
  hint?: string
}

withDefaults(defineProps<Props>(), {
  showHint: true,
  hint: '此功能尚未接入后端，当前为静态占位展示。',
})
</script>

<style scoped>
.placeholder-pane {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 48px 24px;
  text-align: center;
  color: var(--md-sys-color-on-surface-variant);
}

.placeholder-icon {
  color: var(--md-sys-color-primary);
}

.placeholder-title {
  font: var(--md-sys-title-large);
  color: var(--md-sys-color-on-surface);
}

.placeholder-desc {
  max-width: 480px;
  font: var(--md-sys-body-medium);
}

.placeholder-hint {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 8px 16px;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-tertiary-container);
  color: var(--md-sys-color-on-tertiary-container);
  font: var(--md-sys-label-medium);
}
</style>
