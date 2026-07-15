<template>
  <ol class="event-timeline" :aria-label="ariaLabel">
    <li
      v-for="(event, index) in events"
      :key="event.id ?? index"
      class="event-timeline__item"
    >
      <span class="event-timeline__dot" aria-hidden="true" />
      <div class="event-timeline__content">
        <div class="event-timeline__header">
          <span class="event-timeline__title">{{ event.title }}</span>
          <time class="event-timeline__time" :datetime="event.timestamp">{{ event.timestamp }}</time>
        </div>
        <p v-if="event.description" class="event-timeline__description">{{ event.description }}</p>
        <DiagnosticBanner
          v-if="event.diagnostic"
          :code="event.diagnostic.code"
          :message="event.diagnostic.message"
          :retryable="event.diagnostic.retryable"
          severity="info"
        />
      </div>
    </li>
  </ol>
</template>

<script setup lang="ts">
import DiagnosticBanner from './DiagnosticBanner.vue'
import type { DiagnosticViewModel } from '@/types/async'

export interface TimelineEvent {
  id?: string
  title: string
  timestamp: string
  description?: string
  diagnostic?: DiagnosticViewModel
}

interface Props {
  events: TimelineEvent[]
  ariaLabel?: string
}

withDefaults(defineProps<Props>(), {
  ariaLabel: '事件时间线',
})
</script>

<style scoped>
.event-timeline {
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: 0;
  margin: 0;
  padding: 0;
}

.event-timeline__item {
  position: relative;
  display: flex;
  gap: 16px;
  padding: 12px 0;
}

.event-timeline__item:not(:last-child)::before {
  content: '';
  position: absolute;
  top: 28px;
  bottom: -12px;
  left: 7px;
  width: 2px;
  background: var(--md-sys-color-outline-variant);
}

.event-timeline__dot {
  width: 16px;
  height: 16px;
  margin-top: 4px;
  border-radius: 50%;
  background: var(--md-sys-color-primary);
  flex-shrink: 0;
}

.event-timeline__content {
  flex: 1;
  min-width: 0;
}

.event-timeline__header {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  flex-wrap: wrap;
}

.event-timeline__title {
  font: var(--md-sys-title-small);
  color: var(--md-sys-color-on-surface);
}

.event-timeline__time {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  white-space: nowrap;
}

.event-timeline__description {
  margin-top: 4px;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}
</style>
