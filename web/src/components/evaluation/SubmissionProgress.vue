<template>
  <ol class="submission-progress" :aria-label="label">
    <li
      v-for="stage in stages"
      :key="stage.id"
      class="submission-progress__stage"
      :class="`submission-progress__stage--${stage.status}`"
      :aria-current="stage.status === 'current' ? 'step' : undefined"
      :data-stage="stage.id"
    >
      <span class="submission-progress__marker" aria-hidden="true">
        <SvgIcon :name="iconFor(stage.status)" size="sm" />
      </span>
      <span class="submission-progress__text">
        <span class="submission-progress__title">{{ stage.title }}</span>
        <span class="submission-progress__detail">
          <span class="visually-hidden">{{ statusText(stage.status) }}：</span>{{ stage.detail }}
        </span>
      </span>
    </li>
  </ol>
</template>

<script setup lang="ts">
import SvgIcon from '@/components/common/SvgIcon.vue'
import type { SubmissionStage, SubmissionStageStatus } from '@/utils/evaluationProgress'

withDefaults(defineProps<{ stages: SubmissionStage[]; label?: string }>(), { label: '提交进度' })

function iconFor(status: SubmissionStageStatus): string {
  return {
    done: 'check_circle',
    current: 'pending',
    failed: 'error',
    unknown: 'help',
    upcoming: 'radio_button_unchecked',
  }[status]
}

function statusText(status: SubmissionStageStatus): string {
  return { done: '已完成', current: '进行中', failed: '未完成', unknown: '状态未提供', upcoming: '未开始' }[status]
}
</script>

<style scoped>
.submission-progress {
  display: grid;
  grid-template-columns: repeat(5, minmax(0, 1fr));
  gap: 0;
  margin: 0;
  padding: 0;
  list-style: none;
}

.submission-progress__stage {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  min-width: 0;
  padding: 0 var(--space-3) 0 0;
}

/* Connector from this marker toward the next stage. */
.submission-progress__stage:not(:last-child)::after {
  position: absolute;
  top: 11px;
  left: 28px;
  right: 4px;
  height: 2px;
  border-radius: 1px;
  background: var(--md-sys-color-outline-variant);
  content: '';
}

.submission-progress__stage--done:not(:last-child)::after {
  background: var(--md-sys-color-primary);
}

.submission-progress__marker {
  position: relative;
  z-index: 1;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  border-radius: 50%;
  background: var(--md-sys-color-surface);
  color: var(--md-sys-color-outline);
}

.submission-progress__stage--done .submission-progress__marker { color: var(--md-sys-color-primary); }
.submission-progress__stage--current .submission-progress__marker { color: var(--md-sys-color-primary); }
.submission-progress__stage--failed .submission-progress__marker { color: var(--md-sys-color-error); }

.submission-progress__text {
  display: grid;
  gap: var(--space-1);
  min-width: 0;
}

.submission-progress__title {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-label-large);
}

.submission-progress__stage--unknown .submission-progress__title,
.submission-progress__stage--upcoming .submission-progress__title {
  color: var(--md-sys-color-on-surface-variant);
}

.submission-progress__detail {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.submission-progress__stage--current .submission-progress__detail {
  color: var(--md-sys-color-on-surface);
}

@media (max-width: 719px) {
  .submission-progress {
    grid-template-columns: 1fr;
    gap: var(--space-3);
  }

  .submission-progress__stage {
    flex-direction: row;
    align-items: flex-start;
    gap: var(--space-3);
    padding: 0;
  }

  .submission-progress__stage:not(:last-child)::after {
    top: 26px;
    bottom: calc(-1 * var(--space-3));
    left: 11px;
    right: auto;
    width: 2px;
    height: auto;
  }

  .submission-progress__text {
    padding-top: 2px;
  }
}
</style>
