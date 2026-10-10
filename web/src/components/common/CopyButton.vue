<template>
  <span class="copy-button-wrap">
    <button
      type="button"
      class="outlined-button copy-button"
      :disabled="disabled || copied"
      :aria-label="ariaLabel"
      @click="copy"
    >
      <SvgIcon
        :name="copied ? 'check' : 'content_copy'"
        size="sm"
        aria-hidden="true"
      />
      <span>{{ copied ? '已复制' : label }}</span>
    </button>
    <span
      v-if="copyFailed"
      class="copy-fallback"
      role="status"
      aria-live="polite"
    >
      <textarea
        ref="manualCopyInput"
        class="copy-fallback__input"
        :value="text"
        aria-label="手动复制内容"
        readonly
        rows="2"
        @focus="selectManualText"
      />
      <span>自动复制不可用，请按系统复制快捷键复制已选内容。</span>
    </span>
  </span>
</template>

<script setup lang="ts">
import { nextTick, ref } from 'vue'
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
const copyFailed = ref(false)
const manualCopyInput = ref<HTMLTextAreaElement | null>(null)

function selectManualText() {
  const input = manualCopyInput.value
  if (!input) return
  input.focus({ preventScroll: true })
  input.select()
}

async function copy() {
  copied.value = false
  copyFailed.value = false
  try {
    if (typeof navigator.clipboard?.writeText !== 'function') throw new Error('Clipboard API unavailable')
    await navigator.clipboard.writeText(props.text)
    copied.value = true
    window.setTimeout(() => {
      copied.value = false
    }, 2000)
    return
  } catch {
    copyFailed.value = true
  }

  await nextTick()
  selectManualText()
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

.copy-button-wrap {
  display: inline-flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 8px;
}

.copy-fallback {
  display: inline-flex;
  flex-direction: column;
  gap: 4px;
  max-width: min(100%, 560px);
  color: var(--md-sys-color-error);
  font: var(--md-sys-body-small);
}

.copy-fallback__input {
  width: min(560px, 100%);
  min-height: 44px;
  padding: 8px;
  border: 1px solid var(--md-sys-color-error);
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-small);
  resize: vertical;
}

.copy-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
