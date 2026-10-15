<template>
  <SelectRoot
    :model-value="internalValue"
    :disabled="disabled"
    :required="required"
    @update:model-value="onSelect"
  >
    <SelectTrigger
      :id="triggerId"
      :class="['app-select', $attrs.class]"
      :aria-label="ariaLabel"
      :aria-describedby="ariaDescribedby"
      :data-placeholder="internalValue === undefined ? '' : undefined"
    >
      <SelectValue :placeholder="placeholder" class="app-select__value">
        {{ selectedLabel ?? placeholder }}
      </SelectValue>
      <SelectIcon class="app-select__icon">
        <SvgIcon name="expand_more" size="sm" aria-hidden="true" />
      </SelectIcon>
    </SelectTrigger>
    <SelectPortal>
      <SelectContent class="app-select__content" position="popper" :side-offset="4">
        <SelectViewport class="app-select__viewport">
          <SelectItem
            v-for="option in itemOptions"
            :key="option.key"
            :value="option.key"
            :disabled="option.disabled"
            class="app-select__item"
          >
            <SelectItemText>{{ option.label }}</SelectItemText>
            <SelectItemIndicator class="app-select__check">
              <SvgIcon name="check" size="sm" aria-hidden="true" />
            </SelectItemIndicator>
          </SelectItem>
        </SelectViewport>
      </SelectContent>
    </SelectPortal>
  </SelectRoot>
  <!--
    Form/automation bridge: a hidden native <select> mirrors the value both ways
    (Reka's own bubble select is one-way). Hidden from assistive tech and tab order.
  -->
  <select
    v-bind="nativeAttrs"
    class="app-select__native"
    aria-hidden="true"
    tabindex="-1"
    :disabled="disabled"
    :required="required"
    :value="nativeValue"
    @change="onNativeChange"
  >
    <option v-for="option in options" :key="option.key" :value="nativeKey(option.value)" :disabled="option.disabled">{{ option.label }}</option>
  </select>
</template>

<script setup lang="ts">
import { computed, Comment, Fragment, Text, useAttrs, useSlots, type VNode } from 'vue'
import {
  SelectContent, SelectIcon, SelectItem, SelectItemIndicator, SelectItemText,
  SelectPortal, SelectRoot, SelectTrigger, SelectValue, SelectViewport,
} from 'reka-ui'
import SvgIcon from '@/components/common/SvgIcon.vue'

defineOptions({ inheritAttrs: false })

type Value = string | number | null | undefined

const props = defineProps<{
  modelValue?: Value
  disabled?: boolean
  required?: boolean
  placeholder?: string
}>()
const emit = defineEmits<{ 'update:modelValue': [value: Value]; change: [value: Value] }>()
const attrs = useAttrs()
const slots = useSlots()

const EMPTY = '__app_select_empty__'
const keyOf = (value: Value) => (value === '' || value == null ? EMPTY : String(value))

interface Option { key: string; value: Value; label: string; disabled: boolean }

function textOf(nodes: unknown): string {
  if (typeof nodes === 'string') return nodes
  if (Array.isArray(nodes)) return nodes.map(textOf).join('')
  const node = nodes as VNode | null
  if (!node) return ''
  if (node.type === Text) return String(node.children ?? '')
  if (node.type === Comment) return ''
  if (node.children && typeof node.children === 'object' && 'default' in node.children) return textOf((node.children as { default: () => unknown }).default())
  return textOf(node.children)
}

function collect(nodes: VNode[], out: Option[]) {
  for (const node of nodes) {
    if (node.type === Fragment && Array.isArray(node.children)) collect(node.children as VNode[], out)
    else if (node.type === 'option') {
      const p = (node.props ?? {}) as Record<string, unknown>
      const value = ('value' in p ? p.value : textOf(node.children).trim()) as Value
      const disabled = p.disabled === '' || p.disabled === true || p.disabled === 'true'
      out.push({ key: keyOf(value), value, label: textOf(node.children).trim(), disabled })
    }
  }
}

const options = computed<Option[]>(() => {
  const out: Option[] = []
  collect(slots.default?.() ?? [], out)
  return out
})

/* A disabled empty option is a placeholder prompt, not a choice. */
const placeholderOption = computed(() => options.value.find((o) => o.key === EMPTY && o.disabled))
const itemOptions = computed(() => options.value.filter((o) => o !== placeholderOption.value))
const placeholder = computed(() => props.placeholder ?? placeholderOption.value?.label ?? '请选择')

const internalValue = computed(() => {
  const key = keyOf(props.modelValue)
  return itemOptions.value.some((o) => o.key === key) ? key : undefined
})
const nativeKey = (value: Value) => (value == null ? '' : String(value))
const nativeValue = computed(() => nativeKey(props.modelValue))
const selectedLabel = computed(() => itemOptions.value.find((o) => o.key === internalValue.value)?.label)

const triggerId = computed(() => attrs.id as string | undefined)
const ariaLabel = computed(() => attrs['aria-label'] as string | undefined)
const ariaDescribedby = computed(() => attrs['aria-describedby'] as string | undefined)
const nativeAttrs = computed(() => {
  const rest: Record<string, unknown> = { ...attrs }
  delete rest.class
  delete rest.id
  return rest
})

function commit(key: string) {
  const option = options.value.find((o) => o.key === key)
  const value = option ? option.value : key === EMPTY ? '' : key
  emit('update:modelValue', value)
  emit('change', value)
}

function onSelect(key: unknown) {
  if (typeof key === 'string') commit(key)
}

function onNativeChange(event: Event) {
  const raw = (event.target as HTMLSelectElement).value
  const option = options.value.find((o) => nativeKey(o.value) === raw)
  commit(option ? option.key : keyOf(raw))
}
</script>

<style>
.app-select {
  display: inline-flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-2);
  min-width: 0;
  width: 100%;
  height: 40px;
  padding: 0 var(--space-2) 0 var(--space-3);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
  text-align: start;
  cursor: pointer;
  transition: border-color var(--motion-duration-fast) var(--motion-ease-out), box-shadow var(--motion-duration-fast) var(--motion-ease-out);
}

.app-select:not(:disabled):active {
  transform: none;
}

.app-select:hover:not(:disabled) {
  border-color: var(--md-sys-color-outline);
}

.app-select[data-state='open'],
.app-select:focus-visible {
  border-color: var(--md-sys-color-primary);
  box-shadow: 0 0 0 1px var(--md-sys-color-primary);
  outline: none;
}

.app-select:disabled {
  background: var(--md-sys-color-surface-container-low);
  color: color-mix(in srgb, var(--md-sys-color-on-surface) 38%, transparent);
}

.app-select[data-placeholder] .app-select__value {
  color: var(--md-sys-color-on-surface-variant);
}

.app-select__value {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.app-select__icon {
  flex-shrink: 0;
  color: var(--md-sys-color-on-surface-variant);
  transition: transform var(--motion-duration-base) var(--motion-ease-out);
}

.app-select[data-state='open'] .app-select__icon {
  transform: rotate(180deg);
}

.app-select__content {
  z-index: 1000;
  min-width: var(--reka-select-trigger-width);
  max-height: min(360px, var(--reka-select-content-available-height));
  overflow: hidden;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-lowest);
  box-shadow: var(--md-sys-elevation-3);
  transform-origin: var(--reka-select-content-transform-origin);
  animation: app-select-in var(--motion-duration-fast) var(--motion-ease-out);
}

.app-select__viewport {
  padding: var(--space-1);
}

.app-select__item {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
  min-height: 36px;
  padding: var(--space-2) var(--space-3);
  border-radius: var(--md-sys-shape-small);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
  cursor: pointer;
  user-select: none;
  outline: none;
}

.app-select__item[data-highlighted] {
  background: color-mix(in srgb, var(--md-sys-color-on-surface) 8%, transparent);
}

.app-select__item[data-state='checked'] {
  color: var(--md-sys-color-primary);
  font-weight: 500;
}

.app-select__item[data-disabled] {
  color: color-mix(in srgb, var(--md-sys-color-on-surface) 38%, transparent);
  cursor: not-allowed;
}

.app-select__check {
  display: inline-flex;
}

.app-select__native {
  position: absolute;
  width: 1px;
  height: 1px;
  overflow: hidden;
  clip: rect(0 0 0 0);
  white-space: nowrap;
  pointer-events: none;
  opacity: 0;
}

@keyframes app-select-in {
  from {
    opacity: 0;
    transform: scale(0.97) translateY(-2px);
  }
}
</style>
