<template>
  <div v-if="notes.length || record?.readError" class="task-notes">
    <p v-for="note in notes" :key="note" class="task-notes__note">{{ note }}</p>
    <p v-if="record?.readError" class="task-notes__error" role="status" data-testid="task-read-error">
      <span>{{ record.readError.message }}</span>
      <span v-if="record.readAt">当前显示的是 {{ formatTimestamp(record.readAt) }} 读取的状态，页面会继续自动更新。</span>
      <span class="task-notes__code">诊断码 <code>{{ record.readError.code }}</code></span>
    </p>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { EvaluationTaskRecord } from '@/composables/useEvaluationTasks'
import { readEvaluationTask } from '@/utils/evaluationProgress'
import { formatTimestamp } from '@/utils/format'

/**
 * Attempt facts and read failures for one evaluation task. The reason comes
 * first; the diagnostic code follows for support.
 */
const props = defineProps<{ record?: EvaluationTaskRecord | null }>()

const notes = computed(() => {
  const fact = props.record?.fact
  if (fact?.kind !== 'progress') return []
  const reading = readEvaluationTask(fact.progress)
  if (reading.terminal) return []
  const notes: string[] = []
  if (reading.attempt !== null && reading.attempt > 1) notes.push(`当前是这一步的第 ${reading.attempt} 次尝试。`)
  const { count, more } = reading.earlierAttempts
  if (count > 0) notes.push(`此前有 ${count}${more ? ' 次以上' : ' 次'}尝试没有完成，未完成的尝试不计分，也不会覆盖当前进度。`)
  if (reading.status === 'current' && reading.reason) notes.push(`最近一次记录的原因：${reading.reason}。`)
  return notes
})
</script>

<style scoped>
.task-notes {
  display: grid;
  gap: 4px;
}

.task-notes__note,
.task-notes__error {
  margin: 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.task-notes__error {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 8px;
  color: var(--md-sys-color-on-surface);
}
</style>
