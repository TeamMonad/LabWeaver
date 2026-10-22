<template>
  <DialogRoot :open="open" @update:open="onOpenChange">
    <DialogPortal>
      <DialogOverlay class="evidence-scrim" />
      <DialogContent
        as="aside"
        class="evidence-sheet"
        :aria-describedby="undefined"
        @escape-key-down="guardDismiss"
        @interact-outside="guardDismiss"
        @open-auto-focus="onOpenAutoFocus"
      >
        <header class="evidence-sheet__header">
          <DialogTitle class="evidence-sheet__title">{{ title }}</DialogTitle>
          <DialogClose
            v-if="dismissible"
            class="icon-button"
            aria-label="关闭证据侧栏"
          >
            <SvgIcon name="close" size="md" aria-label="关闭" />
          </DialogClose>
        </header>
        <div class="evidence-sheet__content">
          <slot />
        </div>
      </DialogContent>
    </DialogPortal>
  </DialogRoot>
</template>

<script setup lang="ts">
import { DialogClose, DialogContent, DialogOverlay, DialogPortal, DialogRoot, DialogTitle } from 'reka-ui'
import SvgIcon from './SvgIcon.vue'

interface Props {
  open: boolean
  title: string
  dismissible?: boolean
}

const props = withDefaults(defineProps<Props>(), {
  dismissible: true,
})

const emit = defineEmits<{
  close: []
}>()

// Reka Dialog supplies the focus trap, Escape/outside dismissal, scroll lock and
// focus return to the opener; a non-dismissible sheet blocks both dismiss paths.
function onOpenChange(next: boolean) {
  if (!next && props.dismissible) emit('close')
}

function guardDismiss(event: Event) {
  if (!props.dismissible) event.preventDefault()
}

function onOpenAutoFocus(event: Event) {
  if (props.dismissible) return
  // Without a close button keep focus on the sheet itself rather than the first slotted control.
  event.preventDefault()
  ;(event.target as HTMLElement | null)?.focus?.()
}
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
  outline: none;
  transform: translateX(0);
  transition: transform var(--motion-duration-slow) var(--motion-ease-drawer);
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
