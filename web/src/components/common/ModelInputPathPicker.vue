<template>
  <section
    class="model-input-path-picker"
    :aria-labelledby="headingId"
    data-testid="model-input-path-picker"
  >
    <div class="picker-heading">
      <div>
        <h4 :id="headingId">选择送入模型的材料文件</h4>
        <p class="picker-note">
          必须显式勾选至少一份材料包内的精确相对路径；不会默认选择整包。HTTP 202 仅表示任务已受理，不代表模型已成功。
        </p>
      </div>
      <div class="picker-actions">
        <button
          type="button"
          class="text-button"
          data-testid="model-input-clear-selection"
          :disabled="modelValue.length === 0"
          @click="emit('update:modelValue', [])"
        >
          清除选择
        </button>
      </div>
    </div>

    <p
      v-if="files.length === 0"
      class="picker-empty"
      role="status"
    >
      当前材料包没有可选择的文件。
    </p>

    <ul
      v-else
      class="picker-list"
      role="group"
      :aria-label="`材料文件，已选 ${modelValue.length} / ${files.length}`"
    >
      <li
        v-for="file in files"
        :key="file.path"
        class="picker-row"
      >
        <label class="picker-label">
          <input
            type="checkbox"
            :data-testid="`model-input-path-${file.path}`"
            :checked="isChecked(file.path)"
            @change="onToggle(file.path, ($event.target as HTMLInputElement).checked)"
          >
          <span class="picker-path">{{ file.path }}</span>
          <span
            v-if="fileSizeLabel(file)"
            class="picker-size"
          >{{ fileSizeLabel(file) }}</span>
        </label>
      </li>
    </ul>

    <p
      class="picker-summary"
      :class="{ 'picker-summary--warn': modelValue.length === 0 && files.length > 0 }"
      role="status"
      data-testid="model-input-selection-summary"
    >
      <template v-if="files.length === 0">无可选文件</template>
      <template v-else-if="modelValue.length === 0">尚未选择文件，启动按钮将保持禁用</template>
      <template v-else>已选择 {{ modelValue.length }} / {{ files.length }} 个文件</template>
    </p>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { formatBytes } from '@/utils/format'
import { buildSelectedPaths } from '@/utils/modelInputSelection'

export type ModelInputPathPickerFile = {
  path: string
  object?: { sizeBytes?: number }
}

const props = defineProps<{
  files: readonly ModelInputPathPickerFile[]
  modelValue: readonly string[]
}>()

const emit = defineEmits<{
  'update:modelValue': [paths: string[]]
}>()

const headingId = computed(() => 'model-input-path-picker-heading')

function isChecked(path: string): boolean {
  return props.modelValue.includes(path)
}

function onToggle(path: string, checked: boolean) {
  const allowed = new Set(props.files.map((file) => file.path))
  if (!allowed.has(path)) return
  const next = new Set(buildSelectedPaths(props.modelValue))
  if (checked) next.add(path)
  else next.delete(path)
  emit('update:modelValue', buildSelectedPaths([...next]))
}

function fileSizeLabel(file: ModelInputPathPickerFile): string | null {
  const size = file.object?.sizeBytes
  if (typeof size !== 'number' || !Number.isFinite(size) || size < 0) return null
  return formatBytes(size)
}
</script>

<style scoped>
.model-input-path-picker {
  display: grid;
  gap: 12px;
  padding: 14px 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.picker-heading {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
}

.picker-heading h4 {
  margin: 0 0 4px;
  font: var(--md-sys-title-small);
  color: var(--md-sys-color-on-surface);
}

.picker-note {
  margin: 0;
  max-width: 52rem;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.picker-actions {
  display: inline-flex;
  gap: 8px;
}

.picker-empty,
.picker-summary {
  margin: 0;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.picker-summary--warn {
  color: var(--md-sys-color-error);
}

.picker-list {
  display: grid;
  gap: 6px;
  margin: 0;
  padding: 0;
  list-style: none;
  max-height: 240px;
  overflow: auto;
}

.picker-row {
  margin: 0;
}

.picker-label {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  align-items: center;
  gap: 10px;
  padding: 8px 10px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface);
  cursor: pointer;
  font: var(--md-sys-body-small);
}

.picker-label input {
  width: 16px;
  height: 16px;
  margin: 0;
  accent-color: var(--md-sys-color-primary);
}

.picker-path {
  overflow-wrap: anywhere;
  color: var(--md-sys-color-on-surface);
  font-family: var(--md-sys-font-mono, ui-monospace, monospace);
}

.picker-size {
  color: var(--md-sys-color-on-surface-variant);
  white-space: nowrap;
}

.text-button {
  display: inline-flex;
  align-items: center;
  min-height: 36px;
  padding: 0 12px;
  border: 0;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.text-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
