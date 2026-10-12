<template>
  <CollapsibleRoot v-model:open="open" class="cap-picker">
    <div class="cap-picker__summary">
      <span class="cap-picker__label">{{ label }}</span>
      <span class="cap-picker__selected">
        <template v-if="model.length">
          <code v-for="cap in model" :key="cap" class="cap-chip">{{ cap }}</code>
        </template>
        <span v-else class="muted">无</span>
      </span>
      <CollapsibleTrigger class="text-button">{{ open ? '收起' : '编辑' }}</CollapsibleTrigger>
    </div>
    <CollapsibleContent>
      <p v-if="available.length === 0" class="muted">平台允许清单为空，无可选 capability。</p>
      <div v-else class="cap-grid" role="group" :aria-label="label">
        <label v-for="cap in available" :key="cap" class="check">
          <input v-model="model" type="checkbox" :value="cap" />
          <code>{{ cap }}</code>
        </label>
      </div>
    </CollapsibleContent>
  </CollapsibleRoot>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { CollapsibleContent, CollapsibleRoot, CollapsibleTrigger } from 'reka-ui'
import { LINUX_CAPABILITIES, type LinuxCapability } from '@/utils/platformConfiguration'

const props = defineProps<{ label: string; options?: readonly LinuxCapability[] }>()
const model = defineModel<LinuxCapability[]>({ required: true })
const open = ref(false)

/** Selected values stay visible even if a narrower option list no longer contains them, so they can be removed. */
const available = computed<readonly LinuxCapability[]>(() => {
  if (!props.options) return LINUX_CAPABILITIES
  return [...new Set([...props.options, ...model.value])]
})
</script>

<style scoped>
.cap-picker { display: grid; gap: 8px; }
.cap-picker__summary { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.cap-picker__label { font: var(--md-sys-label-medium); color: var(--md-sys-color-on-surface-variant); }
.cap-picker__selected { display: inline-flex; flex-wrap: wrap; gap: 4px; flex: 1; }
.cap-chip { padding: 2px 6px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-variant); font-size: 12px; }
.cap-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(170px, 1fr)); gap: 6px 12px; }
.check { display: inline-flex; align-items: center; gap: 6px; font: var(--md-sys-body-small); }
.muted { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.text-button { padding: 0; border: 0; background: transparent; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); cursor: pointer; }
</style>
