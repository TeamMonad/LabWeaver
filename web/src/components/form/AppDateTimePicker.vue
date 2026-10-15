<template>
  <!-- Reka renders a hidden native datetime-local inside forms; listening here makes it two-way. -->
  <span class="app-datetime-root" @input="onNative" @change="onNative">
  <DatePickerRoot
    :model-value="dateValue"
    granularity="minute"
    :hour-cycle="24"
    locale="zh-CN"
    :disabled="disabled"
    :required="required"
    @update:model-value="onPick"
  >
    <DatePickerField v-slot="{ segments }" :class="['app-datetime', $attrs.class]" :aria-label="ariaLabel">
      <div class="app-datetime__segments">
        <template v-for="item in segments" :key="item.part">
          <DatePickerInput v-if="item.part === 'literal'" :part="item.part" class="app-datetime__literal">
            {{ item.value }}
          </DatePickerInput>
          <DatePickerInput v-else :part="item.part" class="app-datetime__segment">
            {{ item.value }}
          </DatePickerInput>
        </template>
      </div>
      <DatePickerTrigger class="app-datetime__trigger" aria-label="打开日历">
        <SvgIcon name="calendar_month" size="sm" aria-hidden="true" />
      </DatePickerTrigger>
    </DatePickerField>
    <DatePickerContent class="app-datetime__content" :side-offset="4" align="start">
      <DatePickerCalendar v-slot="{ weekDays, grid }" class="app-calendar">
        <DatePickerHeader class="app-calendar__header">
          <DatePickerPrev class="icon-button app-calendar__nav" aria-label="上个月">
            <SvgIcon name="chevron_left" size="sm" aria-hidden="true" />
          </DatePickerPrev>
          <DatePickerHeading class="app-calendar__heading" />
          <DatePickerNext class="icon-button app-calendar__nav" aria-label="下个月">
            <SvgIcon name="chevron_right" size="sm" aria-hidden="true" />
          </DatePickerNext>
        </DatePickerHeader>
        <DatePickerGrid v-for="month in grid" :key="month.value.toString()" class="app-calendar__grid">
          <DatePickerGridHead>
            <DatePickerGridRow class="app-calendar__row">
              <DatePickerHeadCell v-for="day in weekDays" :key="day" class="app-calendar__head-cell">{{ day }}</DatePickerHeadCell>
            </DatePickerGridRow>
          </DatePickerGridHead>
          <DatePickerGridBody>
            <DatePickerGridRow v-for="(week, index) in month.rows" :key="`week-${index}`" class="app-calendar__row">
              <DatePickerCell v-for="day in week" :key="day.toString()" :date="day">
                <DatePickerCellTrigger :day="day" :month="month.value" class="app-calendar__day" />
              </DatePickerCell>
            </DatePickerGridRow>
          </DatePickerGridBody>
        </DatePickerGrid>
      </DatePickerCalendar>
    </DatePickerContent>
  </DatePickerRoot>
    <!--
      Two-way native bridge for forms and automation (Reka's own hidden input,
      rendered only inside <form>, is one-way). Hidden from AT and tab order.
    -->
    <input
      v-bind="nativeAttrs"
      data-app-datetime
      class="app-datetime__native"
      type="datetime-local"
      aria-hidden="true"
      tabindex="-1"
      :value="modelValue ?? ''"
      :disabled="disabled"
      :required="required"
    >
  </span>
</template>

<script setup lang="ts">
import { computed, useAttrs } from 'vue'
import { CalendarDateTime, parseDateTime, type DateValue } from '@internationalized/date'
import {
  DatePickerCalendar, DatePickerCell, DatePickerCellTrigger, DatePickerContent, DatePickerField,
  DatePickerGrid, DatePickerGridBody, DatePickerGridHead, DatePickerGridRow, DatePickerHeadCell,
  DatePickerHeader, DatePickerHeading, DatePickerInput, DatePickerNext, DatePickerPrev,
  DatePickerRoot, DatePickerTrigger,
} from 'reka-ui'
import SvgIcon from '@/components/common/SvgIcon.vue'

defineOptions({ inheritAttrs: false })

/** v-model uses the same `YYYY-MM-DDTHH:mm` string as `<input type="datetime-local">`. */
const props = defineProps<{ modelValue?: string | null; disabled?: boolean; required?: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()
const attrs = useAttrs()

const ariaLabel = computed(() => attrs['aria-label'] as string | undefined)
const nativeAttrs = computed(() => {
  const rest: Record<string, unknown> = { ...attrs }
  delete rest.class
  return rest
})

const dateValue = computed<CalendarDateTime | undefined>(() => {
  if (!props.modelValue) return undefined
  try {
    return parseDateTime(props.modelValue)
  } catch {
    return undefined
  }
})

const pad = (n: number) => String(n).padStart(2, '0')
function format(value: DateValue): string {
  const v = value as CalendarDateTime
  return `${v.year}-${pad(v.month)}-${pad(v.day)}T${pad(v.hour ?? 0)}:${pad(v.minute ?? 0)}`
}

function onPick(value: DateValue | undefined) {
  emit('update:modelValue', value ? format(value) : '')
}

function onNative(event: Event) {
  const target = event.target as HTMLInputElement
  if (!target.hasAttribute('data-app-datetime')) return
  const raw = target.value
  if (raw !== (props.modelValue ?? '')) emit('update:modelValue', raw)
}
</script>

<style>
.app-datetime {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  width: 100%;
  height: var(--density-control-height);
  padding: 0 var(--space-1) 0 var(--space-3);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
  font-variant-numeric: tabular-nums;
  transition: border-color var(--motion-duration-fast) var(--motion-ease-out), box-shadow var(--motion-duration-fast) var(--motion-ease-out);
}

.app-datetime:hover {
  border-color: var(--md-sys-color-outline);
}

.app-datetime:focus-within {
  border-color: var(--md-sys-color-primary);
  box-shadow: 0 0 0 1px var(--md-sys-color-primary);
}

.app-datetime[data-disabled] {
  background: var(--md-sys-color-surface-container-low);
  color: color-mix(in srgb, var(--md-sys-color-on-surface) 38%, transparent);
}

.app-datetime__segments {
  display: flex;
  flex: 1;
  align-items: center;
  min-width: 0;
  overflow: hidden;
}

.app-datetime__segment {
  padding: 0 2px;
  border-radius: var(--md-sys-shape-extra-small);
  outline: none;
  caret-color: transparent;
}

.app-datetime__segment:focus {
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
}

.app-datetime__segment[data-placeholder] {
  color: var(--md-sys-color-on-surface-variant);
}

.app-datetime__literal {
  color: var(--md-sys-color-on-surface-variant);
}

.app-datetime__trigger {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 32px;
  height: 32px;
  border: 0;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
}

.app-datetime__trigger:hover {
  background: color-mix(in srgb, currentColor 8%, transparent);
}

.app-datetime__content {
  z-index: 1000;
  padding: var(--space-3);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-large);
  background: var(--md-sys-color-surface-container-lowest);
  box-shadow: var(--md-sys-elevation-3);
  transform-origin: var(--reka-popover-content-transform-origin);
  animation: app-datetime-in var(--motion-duration-base) var(--motion-ease-out);
}

.app-calendar__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: var(--space-2);
}

.app-calendar__heading {
  font: var(--md-sys-title-small);
}

.app-calendar__nav {
  width: 32px;
  height: 32px;
}

.app-calendar__grid {
  border-collapse: collapse;
}

.app-calendar__row {
  display: grid;
  grid-template-columns: repeat(7, 36px);
}

.app-calendar__head-cell {
  padding-bottom: var(--space-1);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-small);
  text-align: center;
}

.app-calendar__day {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 36px;
  height: 36px;
  border-radius: var(--md-sys-shape-full);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
  font-variant-numeric: tabular-nums;
  cursor: pointer;
  outline: none;
  transition: background-color var(--motion-duration-fast) var(--motion-ease-out);
}

.app-calendar__day:hover,
.app-calendar__day[data-highlighted],
.app-calendar__day:focus-visible {
  background: color-mix(in srgb, var(--md-sys-color-on-surface) 8%, transparent);
}

.app-calendar__day[data-today] {
  box-shadow: inset 0 0 0 1px var(--md-sys-color-primary);
}

.app-calendar__day[data-selected] {
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
}

.app-calendar__day[data-outside-view] {
  color: var(--md-sys-color-on-surface-variant);
  opacity: 0.5;
}

.app-calendar__day[data-disabled],
.app-calendar__day[data-unavailable] {
  opacity: 0.38;
  cursor: not-allowed;
}

.app-datetime__native {
  position: absolute;
  width: 1px;
  height: 1px;
  overflow: hidden;
  clip: rect(0 0 0 0);
  pointer-events: none;
  opacity: 0;
}

.app-datetime-root {
  display: block;
  position: relative;
}

@keyframes app-datetime-in {
  from {
    opacity: 0;
    transform: scale(0.97) translateY(-2px);
  }
}
</style>
