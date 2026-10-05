<template>
  <section
    class="run-history md-card"
    data-testid="project-agent-run-history"
    :aria-labelledby="headingId"
  >
    <div class="section-heading">
      <div>
        <h3 :id="headingId">{{ scope === 'work' ? '项目 Work 任务历史' : '实验任务历史' }}</h3>
        <p>{{ scope === 'work' ? '从当前项目重新打开已有的模板生成或 Work 配置任务。' : '从当前项目重新打开已有的实验候选生成任务。' }}</p>
      </div>
      <button
        type="button"
        class="icon-button"
        aria-label="刷新项目任务历史"
        :disabled="history.kind === 'loading'"
        @click="loadHistory(historyPage)"
      >
        <SvgIcon name="refresh" size="sm" aria-hidden="true" />
      </button>
    </div>

    <AsyncStateView
      :state="history"
      :empty-text="emptyText"
      loading-text="加载项目任务历史…"
      @retry="loadHistory(historyPage)"
    >
      <template #success="{ data }">
        <div v-if="filteredItems.length === 0" class="history-empty" role="status">
          {{ filteredEmptyText }}
        </div>
        <ul v-else class="run-history-list">
          <li v-for="item in filteredItems" :key="item.id" class="run-history-item">
            <div class="run-history-item__main">
              <div class="run-history-item__heading">
                <strong>{{ purposeLabel(item.purpose) }}</strong>
                <span class="state-chip" :class="`state-chip--${item.state}`">{{ agentRunStateLabel(item.state) }}</span>
              </div>
              <p>创建于 {{ formatTimestamp(item.createdAt) }} · 更新于 {{ formatTimestamp(item.updatedAt) }}</p>
              <details class="advanced-details">
                <summary>查看任务标识</summary>
                <code>{{ item.id }}</code>
              </details>
            </div>
            <button type="button" class="outlined-button" @click="emit('open', item)">
              打开任务
            </button>
          </li>
        </ul>
        <div v-if="data.items.length > 0" class="run-history-pagination">
          <button
            type="button"
            class="text-button"
            :disabled="data.page <= 1 || history.kind === 'loading'"
            @click="loadHistory(data.page - 1)"
          >
            上一页
          </button>
          <span>第 {{ data.page }} 页</span>
          <button
            type="button"
            class="text-button"
            :disabled="!data.hasMore || history.kind === 'loading'"
            @click="loadHistory(data.page + 1)"
          >
            下一页
          </button>
        </div>
      </template>
    </AsyncStateView>
  </section>
</template>

<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from 'vue'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { listProjectAgentRuns } from '@/generated/contracts'
import type { AgentRunHistoryItem, AgentRunHistoryPageSchema, AgentRunPurpose } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'
import { agentRunStateLabel } from '@/utils/stateLabels'
import { formatTimestamp } from '@/utils/format'

const props = defineProps<{
  projectId: string | null
  scope: 'work' | 'experiment'
}>()

const emit = defineEmits<{
  open: [item: AgentRunHistoryItem]
}>()

const history = ref<AsyncState<AgentRunHistoryPageSchema>>({ kind: 'idle' })
const historyPage = ref(1)
let historyGeneration = 0

const headingId = computed(() => `project-agent-run-history-${props.scope}`)
const emptyText = computed(() => props.scope === 'work'
  ? '当前项目还没有可恢复的 Work 任务。'
  : '当前项目还没有可恢复的实验任务。')
const filteredEmptyText = computed(() => {
  if (history.value.kind === 'success' && (history.value.data.page > 1 || history.value.data.hasMore)) {
    const taskText = props.scope === 'work' ? '本页没有可恢复的 Work 任务' : '本页没有可恢复的实验任务'
    return `${taskText}${history.value.data.hasMore ? '，可查看下一页' : ''}。`
  }
  return emptyText.value
})
const filteredItems = computed(() => history.value.kind === 'success'
  ? history.value.data.items.filter(isInScope)
  : [])

function isInScope(item: AgentRunHistoryItem): boolean {
  if (props.scope === 'work') {
    return item.purpose.kind === 'work_configuration'
      || (item.purpose.kind === 'authoring' && item.purpose.environmentClass === 'work')
  }
  return item.purpose.kind === 'authoring' && item.purpose.environmentClass === 'experiment'
}

function purposeLabel(purpose: AgentRunPurpose): string {
  if (purpose.kind === 'work_configuration') return 'Work 配置任务'
  return purpose.environmentClass === 'work' ? 'Work 模板生成' : '实验候选生成'
}

async function loadHistory(page = historyPage.value) {
  const projectId = props.projectId
  const generation = ++historyGeneration
  const requestedPage = Math.max(1, page)
  historyPage.value = requestedPage
  if (!projectId) {
    history.value = { kind: 'idle' }
    return
  }

  history.value = { kind: 'loading', message: '加载项目任务历史…' }
  const result = await listProjectAgentRuns({
    path: { projectId },
    query: { page: requestedPage, pageSize: 100 },
  })
  if (generation !== historyGeneration || props.projectId !== projectId) return
  if (result.error) {
    const problem = extractProblemDetails(result.error)
    history.value = {
      kind: 'error',
      diagnostic: makeDiagnostic(
        problem?.diagnosticCode ?? 'PROJECT_AGENT_RUN_HISTORY_LOAD_FAILED',
        problem?.detail ?? '加载项目任务历史失败',
        problem?.retryable ?? true,
      ),
    }
    return
  }
  historyPage.value = result.data.page
  history.value = { kind: 'success', data: result.data }
}

watch(() => props.projectId, () => void loadHistory(1), { immediate: true })

onUnmounted(() => {
  historyGeneration += 1
})
</script>

<style scoped>
.run-history { display: grid; gap: 14px; padding: 18px 20px; }
.section-heading { display: flex; justify-content: space-between; align-items: flex-start; gap: 16px; }
.section-heading h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.section-heading p { margin: 6px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); line-height: 1.5; }
.run-history-list { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.run-history-item { display: flex; align-items: flex-start; justify-content: space-between; gap: 14px; padding: 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.run-history-item__main { min-width: 0; display: grid; gap: 5px; }
.run-history-item__heading { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; color: var(--md-sys-color-on-surface); }
.run-history-item__main p { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.history-empty { padding: 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.run-history-pagination { display: flex; align-items: center; justify-content: flex-end; gap: 12px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
.outlined-button, .text-button { display: inline-flex; justify-content: center; align-items: center; gap: 8px; min-height: 40px; padding: 0 17px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-large); cursor: pointer; }
.outlined-button { border: 1px solid var(--md-sys-color-outline); background: transparent; color: var(--md-sys-color-primary); }
.text-button { min-height: 32px; border: 0; background: transparent; color: var(--md-sys-color-primary); }
.outlined-button:disabled, .text-button:disabled { opacity: .5; cursor: not-allowed; }
.state-chip { display: inline-flex; width: fit-content; padding: 3px 8px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.state-chip--succeeded { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--failed, .state-chip--cancelled { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
@media (max-width: 640px) {
  .run-history-item { flex-direction: column; }
  .run-history-item .outlined-button { width: 100%; }
}
</style>
