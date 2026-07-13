<template>
  <div class="placeholder-pane">
    <div class="placeholder-icon" aria-hidden="true">{{ icon }}</div>
    <h3 class="placeholder-title">{{ title }}</h3>
    <p class="placeholder-desc">{{ description }}</p>
    <div v-if="showMock" class="placeholder-mock">
      <MockBadge />
      <p class="placeholder-mock-hint">{{ mockHint }}</p>
    </div>
  </div>
</template>

<script setup lang="ts">
import MockBadge from './MockBadge.vue'

interface Props {
  icon: string
  title: string
  description: string
  showMock?: boolean
  mockHint?: string
}

withDefaults(defineProps<Props>(), {
  showMock: false,
  mockHint: '此功能尚未接入后端，当前为静态占位展示。',
})
</script>

<style scoped>
.placeholder-pane {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  min-height: 260px;
  text-align: center;
  color: var(--md-sys-color-on-surface-variant);
}

.placeholder-icon {
  width: 64px;
  height: 64px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--md-sys-shape-large);
  background: var(--md-sys-color-surface-variant);
  font-size: 32px;
  margin-bottom: 16px;
}

.placeholder-title {
  font: var(--md-sys-title-large);
  color: var(--md-sys-color-on-surface);
  margin-bottom: 8px;
}

.placeholder-desc {
  font: var(--md-sys-body-medium);
  max-width: 520px;
  line-height: 1.6;
  margin-bottom: 16px;
}

.placeholder-mock {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
}

.placeholder-mock-hint {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  max-width: 480px;
}
</style>
