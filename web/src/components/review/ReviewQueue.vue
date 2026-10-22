<template>
  <nav class="review-queue md-card" aria-labelledby="queue-heading" data-testid="review-queue">
    <div class="queue-head">
      <h3 id="queue-heading">待审核版本</h3>
      <button type="button" class="icon-button" aria-label="刷新待审核列表" :disabled="history.kind === 'loading'" @click="load">
        <SvgIcon name="refresh" size="sm" aria-hidden="true" />
      </button>
    </div>
    <AsyncStateView :state="history" loading-text="读取实验生成任务…" @retry="load">
      <template #success="{ data }">
        <p v-if="groups.length === 0" class="queue-empty" role="status">
          这个项目还没有实验生成任务。先在“创建与生成实验”上传材料并生成候选。
          <RouterLink :to="{ path: '/teacher/materials', query: { projectId } }">去上传材料</RouterLink>
        </p>
        <p v-if="data.releaseError" class="queue-note queue-note--warning" role="status" data-testid="queue-release-error">
          没能读取已发布版本（{{ data.releaseError }}），已发布的任务可能仍列在“可以审核”中。点击右上角刷新重试。
        </p>
        <section v-for="group in groups" :key="group.key" class="queue-group" :aria-label="group.label" :data-group="group.key">
          <h4>{{ group.label }}</h4>
          <ul>
            <li v-for="item in group.items" :key="item.id">
              <RouterLink
                v-if="group.key !== 'unfinished'"
                class="queue-item"
                :class="{ 'queue-item--current': item.id === selectedRunId }"
                :aria-current="item.id === selectedRunId ? 'page' : undefined"
                :to="{ path: '/teacher/approvals', query: { projectId, runId: item.id } }"
                :data-run-id="item.id"
              >
                <span class="queue-item__title">实验候选 · {{ formatTimestamp(item.createdAt) }}</span>
                <span class="queue-item__meta">{{ itemMeta(item) }}</span>
              </RouterLink>
              <RouterLink
                v-else
                class="queue-item queue-item--unfinished"
                :to="{ path: '/teacher/materials', query: { projectId, runId: item.id } }"
                :data-run-id="item.id"
              >
                <span class="queue-item__title">实验候选 · {{ formatTimestamp(item.createdAt) }}</span>
                <span class="queue-item__meta">{{ runStateLabel(item.state) }} · 回到材料页沿原任务继续</span>
              </RouterLink>
            </li>
          </ul>
        </section>
        <p v-if="data.hasMore" class="queue-note" role="status">只列出最近 {{ PAGE_SIZE }} 个生成任务。</p>
      </template>
    </AsyncStateView>
  </nav>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { RouterLink } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { fetchEnvironmentTemplateReleases } from '@/composables/useEnvironmentTemplateReleases'
import { listProjectAgentRuns, type AgentRunHistoryItem } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'
import {
  activeReleasesByRun,
  isExperimentAuthoringRun,
  queueGroup,
  QUEUE_GROUP_LABEL,
  runStateLabel,
  type QueueGroup,
  type ReviewQueueSnapshot,
} from '@/utils/authoringReview'
import { formatTimestamp } from '@/utils/format'

const props = defineProps<{ projectId: string; selectedRunId?: string }>()
const emit = defineEmits<{ loaded: [snapshot: ReviewQueueSnapshot] }>()

const PAGE_SIZE = 50
const history = ref<AsyncState<ReviewQueueSnapshot>>({ kind: 'idle' })
let generation = 0

async function load() {
  const id = props.projectId
  const current = ++generation
  history.value = { kind: 'loading' }
  const [runs, releases] = await Promise.all([
    listProjectAgentRuns({ path: { projectId: id }, query: { page: 1, pageSize: PAGE_SIZE } }),
    fetchEnvironmentTemplateReleases(id),
  ])
  if (current !== generation) return
  if (runs.error) {
    const problem = extractProblemDetails(runs.error)
    history.value = { kind: 'error', diagnostic: makeDiagnostic(problem?.diagnosticCode ?? 'AUTHORING_QUEUE_LOAD_FAILED', problem?.detail ?? '读取实验生成任务失败。', problem?.retryable ?? true) }
    return
  }
  const releaseError = releases.error
    ? extractProblemDetails(releases.error)?.diagnosticCode ?? 'AUTHORING_QUEUE_RELEASES_LOAD_FAILED'
    : null
  const snapshot: ReviewQueueSnapshot = {
    items: runs.data.items.filter((item) => item.projectId === id && isExperimentAuthoringRun(item)),
    releasesByRun: releaseError ? new Map() : activeReleasesByRun(releases.items.filter((release) => release.projectId === id)),
    releaseError,
    hasMore: runs.data.hasMore,
  }
  history.value = { kind: 'success', data: snapshot }
  emit('loaded', snapshot)
}

const ORDER: QueueGroup[] = ['review', 'generating', 'unfinished', 'published']
const groups = computed(() => {
  if (history.value.kind !== 'success') return []
  const { items, releasesByRun } = history.value.data
  return ORDER
    .map((key) => ({ key, label: QUEUE_GROUP_LABEL[key], items: items.filter((item) => queueGroup(item.state, releasesByRun.has(item.id)) === key) }))
    .filter((group) => group.items.length > 0)
})

function itemMeta(item: AgentRunHistoryItem): string {
  const release = history.value.kind === 'success' ? history.value.data.releasesByRun.get(item.id) : undefined
  if (release) return `已发布版本 ${release.version} · ${formatTimestamp(release.publishedAt)}`
  return `${runStateLabel(item.state)} · 更新于 ${formatTimestamp(item.updatedAt)}`
}

watch(() => props.projectId, load, { immediate: true })
defineExpose({ load })
</script>

<style scoped>
.review-queue { display: grid; align-content: start; gap: 12px; padding: 16px; }
.queue-head { display: flex; align-items: center; justify-content: space-between; }
.queue-head h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-medium); }
.queue-group h4 { margin: 4px 0 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.queue-group ul { display: grid; gap: 4px; margin: 0; padding: 0; list-style: none; }
.queue-item { display: grid; gap: 2px; padding: 10px 12px; border-radius: var(--md-sys-shape-small); color: var(--md-sys-color-on-surface); text-decoration: none; }
.queue-item:hover { background: var(--md-sys-color-surface-container); }
.queue-item:focus-visible { outline: 2px solid var(--md-sys-color-primary); outline-offset: 1px; }
.queue-item--current { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.queue-item__title { font: var(--md-sys-title-small); }
.queue-item__meta { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.queue-item--unfinished .queue-item__meta { color: var(--md-sys-color-error); }
.queue-empty, .queue-note { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.queue-note { font: var(--md-sys-body-small); }
.queue-note--warning { color: var(--md-sys-color-error); }
.queue-empty a { color: var(--md-sys-color-primary); }
.icon-button { display: inline-grid; place-items: center; width: 36px; height: 36px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
</style>
