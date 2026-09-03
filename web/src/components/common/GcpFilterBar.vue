<template>
  <div class="gcp-filter-bar" role="search" aria-label="数据过滤">
    <div class="filter-box">
      <SvgIcon name="filter_list" size="sm" class="filter-icon" aria-hidden="true" />

      <!-- Active chips -->
      <div v-if="activeChips.length > 0" class="filter-chips">
        <span
          v-for="chip in activeChips"
          :key="`${chip.key}:${chip.value}`"
          class="filter-chip"
        >
          <span class="chip-label">{{ chip.label || chip.key }}:</span>
          <span class="chip-value">{{ chip.displayValue || chip.value }}</span>
          <button
            type="button"
            class="chip-remove"
            :aria-label="`移除过滤: ${chip.label || chip.key}`"
            @click="removeChip(chip)"
          >
            <SvgIcon name="close" size="sm" aria-hidden="true" />
          </button>
        </span>
      </div>

      <!-- Text input -->
      <input
        v-model="searchTerm"
        type="text"
        class="filter-input"
        :placeholder="activeChips.length === 0 ? (placeholder || '过滤表格数据…') : '继续添加过滤关键词…'"
        aria-label="表格过滤搜索"
        @keydown.enter="onEnterKey"
      />

      <!-- Clear all button -->
      <button
        v-if="hasActiveFilter"
        type="button"
        class="clear-all-btn"
        aria-label="清除所有过滤"
        @click="clearAll"
      >
        <SvgIcon name="close" size="sm" aria-hidden="true" />
      </button>
    </div>

    <!-- Quick filter presets (optional) -->
    <div v-if="presets && presets.length > 0" class="filter-presets">
      <button
        v-for="preset in presets"
        :key="preset.label"
        type="button"
        class="preset-btn"
        :class="{ 'preset-btn--active': isPresetActive(preset) }"
        @click="togglePreset(preset)"
      >
        {{ preset.label }}
      </button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import SvgIcon from '@/components/common/SvgIcon.vue'

export interface FilterChip {
  key: string
  label?: string
  value: string
  displayValue?: string
}

export interface FilterPreset {
  label: string
  key: string
  value: string
  displayValue?: string
}

interface Props {
  placeholder?: string
  presets?: FilterPreset[]
  modelValue?: string
}

const props = withDefaults(defineProps<Props>(), {
  placeholder: undefined,
  presets: () => [],
  modelValue: '',
})

const emit = defineEmits<{
  'update:modelValue': [value: string]
  filterChange: [payload: { search: string; chips: FilterChip[] }]
}>()

const searchTerm = ref(typeof props.modelValue === 'string' ? props.modelValue : '')
const activeChips = ref<FilterChip[]>([])

const hasActiveFilter = computed(
  () => activeChips.value.length > 0 || String(searchTerm.value || '').trim().length > 0,
)

watch(searchTerm, (val) => {
  emit('update:modelValue', String(val || ''))
  emitChange()
})

function emitChange() {
  emit('filterChange', {
    search: String(searchTerm.value || '').trim(),
    chips: [...activeChips.value],
  })
}

function removeChip(chip: FilterChip) {
  activeChips.value = activeChips.value.filter((c) => !(c.key === chip.key && c.value === chip.value))
  emitChange()
}

function clearAll() {
  searchTerm.value = ''
  activeChips.value = []
  emitChange()
}

function isPresetActive(preset: FilterPreset): boolean {
  return activeChips.value.some((c) => c.key === preset.key && c.value === preset.value)
}

function togglePreset(preset: FilterPreset) {
  if (isPresetActive(preset)) {
    activeChips.value = activeChips.value.filter((c) => !(c.key === preset.key && c.value === preset.value))
  } else {
    activeChips.value.push({
      key: preset.key,
      label: preset.label,
      value: preset.value,
      displayValue: preset.displayValue || preset.value,
    })
  }
  emitChange()
}

function onEnterKey() {
  const text = searchTerm.value.trim()
  if (!text) return
  // If user types key:value, convert to chip
  if (text.includes(':')) {
    const [key, ...rest] = text.split(':')
    const val = rest.join(':').trim()
    if (key && val) {
      activeChips.value.push({
        key: key.trim(),
        value: val,
      })
      searchTerm.value = ''
      emitChange()
    }
  }
}
</script>

<style scoped>
.gcp-filter-bar {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-bottom: 12px;
}

.filter-box {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
  min-height: 36px;
  padding: 4px 10px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface);
  transition: border-color 0.2s, box-shadow 0.2s;
}

.filter-box:focus-within {
  border-color: var(--md-sys-color-primary);
  box-shadow: 0 0 0 1px var(--md-sys-color-primary);
}

.filter-icon {
  color: var(--md-sys-color-on-surface-variant);
  flex-shrink: 0;
}

.filter-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.filter-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 24px;
  padding: 0 6px 0 8px;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
  font: var(--md-sys-label-small);
}

.chip-label {
  font-weight: 500;
  opacity: 0.8;
}

.chip-value {
  font-weight: 600;
}

.chip-remove {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 16px;
  height: 16px;
  padding: 0;
  border: none;
  border-radius: 50%;
  background: transparent;
  color: inherit;
  cursor: pointer;
}

.chip-remove:hover {
  background: rgba(0, 0, 0, 0.12);
}

.filter-input {
  flex: 1;
  min-width: 140px;
  height: 26px;
  border: none;
  outline: none;
  background: transparent;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface);
}

.clear-all-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  border: none;
  border-radius: 50%;
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  cursor: pointer;
}

.clear-all-btn:hover {
  background: var(--md-sys-color-surface-container-highest);
  color: var(--md-sys-color-on-surface);
}

.filter-presets {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.preset-btn {
  display: inline-flex;
  align-items: center;
  height: 24px;
  padding: 0 8px;
  border-radius: var(--md-sys-shape-full);
  border: 1px solid var(--md-sys-color-outline-variant);
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-small);
  cursor: pointer;
  transition: all 0.15s;
}

.preset-btn:hover {
  background: var(--md-sys-color-surface-container);
  color: var(--md-sys-color-on-surface);
}

.preset-btn--active {
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
  border-color: var(--md-sys-color-primary);
  font-weight: 500;
}
</style>
