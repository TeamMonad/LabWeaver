<template>
  <div class="role-layout">
    <header class="role-header">
      <div class="role-title-row">
        <span class="role-icon" aria-hidden="true">
          <SvgIcon :name="icon" size="lg" />
        </span>
        <div>
          <h1 class="role-title">{{ title }}</h1>
          <p class="role-subtitle">{{ subtitle }}</p>
        </div>
      </div>
    </header>

    <nav class="role-tabs" aria-label="功能标签">
      <RouterLink
        v-for="tab in tabs"
        :key="tab.path"
        :to="tab.path"
        class="role-tab"
        active-class="role-tab-active"
      >
        {{ tab.label }}
      </RouterLink>
    </nav>

    <section class="role-content md-card">
      <slot />
    </section>
  </div>
</template>

<script setup lang="ts">
import { RouterLink } from 'vue-router'
import SvgIcon from '@/components/common/SvgIcon.vue'

interface Tab {
  path: string
  label: string
}

interface Props {
  icon: string
  title: string
  subtitle: string
  tabs: Tab[]
}

defineProps<Props>()
</script>

<style scoped>
.role-layout {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.role-header {
  padding: 8px 0 0;
}

.role-title-row {
  display: flex;
  align-items: center;
  gap: 16px;
}

.role-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 56px;
  height: 56px;
  border-radius: var(--md-sys-shape-large);
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
}

.role-title {
  font: var(--md-sys-headline-small);
  color: var(--md-sys-color-on-surface);
}

.role-subtitle {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
  margin-top: 4px;
}

.role-tabs {
  display: flex;
  align-items: center;
  gap: 8px;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
  overflow-x: auto;
  scrollbar-width: none;
}

.role-tabs::-webkit-scrollbar {
  display: none;
}

.role-tab {
  position: relative;
  display: inline-flex;
  flex: 0 0 auto;
  align-items: center;
  height: 44px;
  padding: 0 20px;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-large);
  text-decoration: none;
  white-space: nowrap;
  transition: color 0.2s;
}

.role-tab:hover {
  color: var(--md-sys-color-on-surface);
}

.role-tab-active {
  color: var(--md-sys-color-primary);
}

.role-tab-active::after {
  content: '';
  position: absolute;
  bottom: 0;
  left: 8px;
  right: 8px;
  height: 3px;
  border-radius: 3px 3px 0 0;
  background: var(--md-sys-color-primary);
}

.role-content {
  min-height: 320px;
  padding: 32px;
}
</style>
