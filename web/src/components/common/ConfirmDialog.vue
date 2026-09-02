<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="confirm-dialog__scrim"
      aria-hidden="true"
      @click="dismissible && $emit('cancel')"
    />
    <dialog
      v-if="open"
      ref="dialogRef"
      class="confirm-dialog"
      role="alertdialog"
      aria-modal="true"
      :aria-labelledby="titleId"
      :aria-describedby="descId"
      open
      @keydown.esc.prevent="dismissible && $emit('cancel')"
      @keydown.tab="trapFocus"
    >
      <div class="confirm-dialog__icon" aria-hidden="true">
        <SvgIcon :name="icon" size="xl" />
      </div>
      <div class="confirm-dialog__content">
        <h2 :id="titleId" class="confirm-dialog__title">{{ title }}</h2>
        <p :id="descId" class="confirm-dialog__description">{{ description }}</p>
      </div>
      <div class="confirm-dialog__actions">
        <button
          v-if="dismissible"
          ref="cancelButtonRef"
          type="button"
          class="text-button"
          @click="$emit('cancel')"
        >
          {{ cancelText }}
        </button>
        <button
          type="button"
          class="filled-button"
          :class="`filled-button--${confirmSeverity}`"
          @click="$emit('confirm')"
        >
          {{ confirmText }}
        </button>
      </div>
    </dialog>
  </Teleport>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
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

defineEmits<{
  confirm: []
  cancel: []
}>()

const instanceId = `lw-confirm-${Math.random().toString(36).slice(2, 10)}`
const titleId = computed(() => `confirm-title-${instanceId}`)
const descId = computed(() => `confirm-desc-${instanceId}`)

const dialogRef = ref<HTMLDialogElement | null>(null)
const cancelButtonRef = ref<HTMLButtonElement | null>(null)

// Focus lands on the cancel action so an accidental Enter can never trigger an
// irreversible confirm (GCP-style destructive-action protection).
watch(
  () => props.open,
  async (open) => {
    if (!open) return
    await nextTick()
    if (props.dismissible) cancelButtonRef.value?.focus()
    else dialogRef.value?.querySelector<HTMLButtonElement>('.filled-button')?.focus()
  },
  { immediate: true },
)

function trapFocus(event: KeyboardEvent) {
  const dialog = dialogRef.value
  if (!dialog) return
  const focusable = Array.from(
    dialog.querySelectorAll<HTMLElement>('button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'),
  )
  if (focusable.length === 0) return
  const first = focusable[0]
  const last = focusable[focusable.length - 1]
  const active = document.activeElement
  if (event.shiftKey && (active === first || !dialog.contains(active))) {
    event.preventDefault()
    last.focus()
  } else if (!event.shiftKey && active === last) {
    event.preventDefault()
    first.focus()
  }
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

const confirmSeverity = computed(() => props.severity)
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
  gap: 16px;
  width: min(400px, calc(100vw - 32px));
  max-height: calc(100vh - 32px);
  margin: 0;
  padding: 24px;
  border: none;
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
  margin-top: 8px;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.confirm-dialog__actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 8px;
}

.text-button,
.filled-button {
  height: 40px;
  padding: 0 24px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.text-button {
  background: transparent;
  color: var(--md-sys-color-primary);
}

.filled-button {
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
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
