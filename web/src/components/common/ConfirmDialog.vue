<template>
  <AlertDialogRoot :open="open">
    <AlertDialogPortal>
      <AlertDialogOverlay
        class="confirm-dialog__scrim"
        @click="dismissible && $emit('cancel')"
      />
      <AlertDialogContent
        class="confirm-dialog"
        @escape-key-down="onEscape"
        @open-auto-focus="onOpenAutoFocus"
      >
        <div class="confirm-dialog__icon" aria-hidden="true">
          <SvgIcon :name="icon" size="xl" />
        </div>
        <div class="confirm-dialog__content">
          <AlertDialogTitle as="h2" class="confirm-dialog__title">{{ title }}</AlertDialogTitle>
          <AlertDialogDescription as="p" class="confirm-dialog__description">{{ description }}</AlertDialogDescription>
        </div>
        <div class="confirm-dialog__actions">
          <AlertDialogCancel
            v-if="dismissible"
            type="button"
            class="text-button"
            @click="$emit('cancel')"
          >
            {{ cancelText }}
          </AlertDialogCancel>
          <button
            ref="confirmButtonRef"
            type="button"
            class="filled-button"
            :class="`filled-button--${severity}`"
            @click="$emit('confirm')"
          >
            {{ confirmText }}
          </button>
        </div>
      </AlertDialogContent>
    </AlertDialogPortal>
  </AlertDialogRoot>
</template>

<script setup lang="ts">
/**
 * Confirmation for consequential actions, built on Reka UI AlertDialog: focus is
 * trapped inside, lands on Cancel so an accidental Enter never confirms, Esc
 * cancels, and focus returns to the opening control when the dialog closes.
 * The dialog is controlled by `open`; it never closes itself, the parent does
 * on `confirm` or `cancel`.
 */
import { computed, ref } from 'vue'
import {
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogOverlay,
  AlertDialogPortal,
  AlertDialogRoot,
  AlertDialogTitle,
} from 'reka-ui'
import SvgIcon from './SvgIcon.vue'

interface Props {
  open: boolean
  title: string
  description: string
  confirmText?: string
  cancelText?: string
  dismissible?: boolean
  severity?: 'error' | 'warning' | 'info'
}

const props = withDefaults(defineProps<Props>(), {
  confirmText: '确认',
  cancelText: '取消',
  dismissible: true,
  severity: 'warning',
})

const emit = defineEmits<{
  confirm: []
  cancel: []
}>()

const confirmButtonRef = ref<HTMLButtonElement | null>(null)

function onEscape(event: KeyboardEvent) {
  // Esc never closes the dialog by itself: a dismissible dialog reports cancel,
  // a non-dismissible one ignores it.
  event.preventDefault()
  if (props.dismissible) emit('cancel')
}

function onOpenAutoFocus(event: Event) {
  // Reka focuses Cancel by default; without Cancel the only action gets focus.
  if (props.dismissible) return
  event.preventDefault()
  confirmButtonRef.value?.focus()
}

const icon = computed(() => {
  switch (props.severity) {
    case 'error':
      return 'error'
    case 'info':
      return 'info'
    default:
      return 'warning'
  }
})
</script>

<style scoped>
.confirm-dialog__scrim {
  position: fixed;
  inset: 0;
  z-index: 2000;
  background: var(--md-sys-color-scrim);
}

.confirm-dialog {
  position: fixed;
  top: 50%;
  left: 50%;
  z-index: 2001;
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  width: min(400px, calc(100vw - 32px));
  max-height: calc(100vh - 32px);
  margin: 0;
  padding: var(--space-6);
  border: none;
  outline: none;
  border-radius: var(--md-sys-shape-extra-large);
  background: var(--md-sys-color-surface-container-high);
  color: var(--md-sys-color-on-surface);
  transform: translate(-50%, -50%);
  box-shadow: var(--md-sys-elevation-3);
}

.confirm-dialog__icon {
  color: var(--md-sys-color-primary);
}

.confirm-dialog__title {
  font: var(--md-sys-headline-small);
}

.confirm-dialog__description {
  margin-top: var(--space-2);
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.confirm-dialog__actions {
  display: flex;
  justify-content: flex-end;
  gap: var(--space-2);
  margin-top: var(--space-2);
}

.filled-button--warning {
  background: var(--md-sys-color-warning-container);
  color: var(--md-sys-color-on-surface);
}

.filled-button--error {
  background: var(--md-sys-color-error);
  color: var(--md-sys-color-on-error);
}
</style>
