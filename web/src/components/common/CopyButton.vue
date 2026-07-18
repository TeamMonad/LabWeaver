<template>
  <button
    type="button"
    class="outlined-button copy-button"
    :disabled="disabled || copied"
    :aria-label="ariaLabel"
    @click="copy"
  >
    <SvgIcon :name="copied ? 'check' : 'content_copy'" size="sm" aria-hidden="true" />
    <span>{{ copied ? '已复制' : label }}</span>
  </button>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import SvgIcon from './SvgIcon.vue'

interface Props {
  text: string
  label?: string
  ariaLabel?: string
  disabled?: boolean
}

const props = withDefaults(defineProps<Props>(), {
  label: '复制',
  ariaLabel: '复制到剪贴板',
  disabled: false,
})

const copied = ref(false)

async function copy() {
  try {
    await navigator.clipboard.writeText(props.text)
    copied.value = true
    window.setTimeout(() => {
      copied.value = false
    }, 2000)
  } catch {
    // Clipboard unavailable (non-secure context or permission denied): keep the
    // command visible for manual selection; no silent fallback.
  }
}
</script>

<style scoped>
.copy-button {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 40px;
  padding: 0 16px;
  border: 1px solid var(--md-sys-color-outline);
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.copy-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
