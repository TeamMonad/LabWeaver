<template>
  <div class="fixture-banner" data-testid="fixture-banner" role="status" aria-live="polite">
    <SvgIcon name="warning" size="sm" aria-hidden="true" />
    <span>FIXTURE MODE — 当前使用确定性本地 fixture，数据并非真实后端。</span>
  </div>
</template>

<script setup lang="ts">
import SvgIcon from '@/components/common/SvgIcon.vue'
</script>

<style scoped>
.fixture-banner {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  height: 32px;
  padding: 0 16px;
  background: var(--md-sys-color-warning-container);
  color: var(--md-sys-color-warning);
  font: var(--md-sys-label-large);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
