<template>
  <fieldset class="model-input-picker" data-testid="model-input-picker" :aria-describedby="summaryId">
    <legend class="picker-legend">选择交给模型的文件</legend>
    <p class="picker-note">
      只有勾选的文件会交给模型读取；未勾选的文件即使在同一目录也不会发送。平台不会自动选择整个材料包。
    </p>

    <p v-if="files.length === 0" class="picker-empty" role="status">
      当前材料包没有文件，无法启动生成。
    </p>
    <template v-else>
      <div class="picker-actions">
        <button
          type="button"
          class="text-button"
          data-testid="model-input-select-all"
          :disabled="disabled || modelValue.length === files.length"
          @click="emit('update:modelValue', files.map((file) => file.path))"
        >
          全部勾选
        </button>
        <button
          type="button"
          class="text-button"
          data-testid="model-input-clear"
          :disabled="disabled || modelValue.length === 0"
          @click="emit('update:modelValue', [])"
        >
          清除勾选
        </button>
      </div>
      <ul class="picker-list">
        <li v-for="file in files" :key="file.path">
          <label class="picker-option">
            <input
              type="checkbox"
              data-testid="model-input-path-option"
              :value="file.path"
              :checked="checkedPaths.has(file.path)"
              :disabled="disabled"
              @change="toggle(file.path, ($event.target as HTMLInputElement).checked)"
            >
            <code class="picker-path">{{ file.path }}</code>
            <span class="picker-size">{{ formatBytes(file.object.sizeBytes) }}</span>
          </label>
        </li>
      </ul>
    </template>

    <p
      :id="summaryId"
      class="picker-summary"
      :class="{ 'picker-summary--missing': modelValue.length === 0 }"
      role="status"
      data-testid="model-input-summary"
    >
      <template v-if="modelValue.length === 0">尚未勾选文件，启动按钮不可用。请至少勾选一个要交给模型的文件。</template>
      <template v-else>已勾选 {{ modelValue.length }} / {{ files.length }} 个文件。</template>
    </p>
  </fieldset>
</template>

<script setup lang="ts">
import { computed, useId } from 'vue'
import type { ProblemPackageSchema } from '@/generated/contracts'
import { formatBytes } from '@/utils/format'

const props = withDefaults(defineProps<{
  files: ProblemPackageSchema['files']
  modelValue: readonly string[]
  disabled?: boolean
}>(), { disabled: false })

const emit = defineEmits<{
  'update:modelValue': [paths: string[]]
}>()

const summaryId = `model-input-summary-${useId()}`
const checkedPaths = computed(() => new Set(props.modelValue))

function toggle(path: string, checked: boolean) {
  const next = new Set(props.modelValue)
  if (checked) next.add(path)
  else next.delete(path)
  emit('update:modelValue', props.files.map((file) => file.path).filter((candidate) => next.has(candidate)))
}
</script>

<style scoped>
.model-input-picker {
  display: grid;
  gap: 10px;
  min-width: 0;
  margin: 0;
  padding: 14px 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.picker-legend {
  padding: 0 4px;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-small);
}

.picker-note,
.picker-empty,
.picker-summary {
  margin: 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.picker-summary--missing {
  color: var(--md-sys-color-error);
}

.picker-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.picker-list {
  display: grid;
  gap: 4px;
  max-height: 260px;
  margin: 0;
  padding: 0;
  overflow: auto;
  list-style: none;
}

.picker-option {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  align-items: center;
  gap: 10px;
  padding: 6px 10px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface);
  cursor: pointer;
  font: var(--md-sys-body-small);
}

.picker-option input {
  width: 16px;
  height: 16px;
  margin: 0;
  accent-color: var(--md-sys-color-primary);
}

.picker-path {
  overflow-wrap: anywhere;
  white-space: pre-wrap;
  color: var(--md-sys-color-on-surface);
}

.picker-size {
  color: var(--md-sys-color-on-surface-variant);
  white-space: nowrap;
}
</style>
