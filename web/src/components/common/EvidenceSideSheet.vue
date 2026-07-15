<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="evidence-scrim"
      aria-hidden="true"
      @click="dismissible && $emit('close')"
    />
    <aside
      v-if="open"
      class="evidence-sheet"
      role="complementary"
      aria-label="证据侧栏"
    >
      <header class="evidence-sheet__header">
        <h2 class="evidence-sheet__title">{{ title }}</h2>
        <button
          v-if="dismissible"
          type="button"
          class="icon-button"
          aria-label="关闭证据侧栏"
          @click="$emit('close')"
        >
          <SvgIcon name="close" size="md" aria-label="关闭" />
        </button>
      </header>
      <div class="evidence-sheet__content">
        <slot />
      </div>
    </aside>
  </Teleport>
</template>

<script setup lang="ts">
import SvgIcon from './SvgIcon.vue'

interface Props {
  open: boolean
  title: string
  dismissible?: boolean
}

withDefaults(defineProps<Props>(), {
  dismissible: true,
})

defineEmits<{
  close: []
}>()
</script>

<style scoped>
.evidence-scrim {
  position: fixed;
  inset: 0;
  z-index: 1300;
  background: var(--md-sys-color-scrim);
}

.evidence-sheet {
  position: fixed;
  top: var(--app-top-bar-height);
  right: 0;
  bottom: 0;
  z-index: 1350;
  display: flex;
  flex-direction: column;
  width: min(var(--evidence-side-sheet-width), 100vw);
  background: var(--md-sys-color-surface);
  border-left: 1px solid var(--md-sys-color-outline-variant);
  box-shadow: var(--md-sys-elevation-3);
  transform: translateX(0);
  transition: transform 0.25s ease;
}

.evidence-sheet__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  height: 64px;
  padding: 0 16px;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.evidence-sheet__title {
  font: var(--md-sys-title-large);
  color: var(--md-sys-color-on-surface);
}

.evidence-sheet__content {
  flex: 1;
  overflow-y: auto;
  padding: 16px;
}

.icon-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 48px;
  height: 48px;
  padding: 0;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  cursor: pointer;
}

.icon-button:hover {
  background: var(--md-sys-color-surface-container-highest);
}

@media (max-width: 839px) {
  .evidence-sheet {
    top: 0;
    width: 100vw;
  }
}

@media (prefers-reduced-motion: reduce) {
  .evidence-sheet {
    transition: none;
  }
}
</style>
