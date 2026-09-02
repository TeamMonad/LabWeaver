<template>
  <div class="my-labs">
    <header class="page-header">
      <h2>我的实验</h2>
      <p class="page-subtitle">查看课程内你创建的环境，进入控制台开始实验。</p>
    </header>

    <DiagnosticBanner
      v-if="isContextMissing"
      code="COURSE_CONTEXT_MISSING"
      message="课程上下文未绑定，无法加载你的实验列表。请通过课程选择器选择课程或联系管理员。"
      :retryable="false"
      severity="error"
    />
    <template v-else>
      <AsyncStateView :state="state" @retry="load">
        <template #success="{ data }">
          <DataTable :columns="columns" :rows="data" aria-label="我的实验环境列表">
            <template #observedState="{ row }">
              <span class="env-state" :class="`env-state--${row.observedState}`">
                {{ environmentStateLabel(row.observedState) }}
              </span>
            </template>
            <template #runtimeKind="{ row }">
              {{ row.runtimeKind === 'container' ? '容器' : '虚拟机' }}
            </template>
            <template #eligibilityExpiresAt="{ row }">
              {{ formatTimestamp(row.eligibilityExpiresAt) }}
            </template>
            <template #actions="{ row }">
              <button
                type="button"
                class="filled-button small"
                @click="openEnvironment(row.id)"
              >
                进入
              </button>
            </template>
          </DataTable>
          <p class="labs-hint" role="status">
            在「环境控制台」页从已发布版本创建新环境；列表为空时请先联系教师发布实验版本。
          </p>
        </template>
      </AsyncStateView>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, onScopeDispose, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useCourseContext } from '@/composables/useCourseContext'
import { listEnvironments } from '@/generated/contracts'
import type { EnvironmentSummary } from '@/generated/contracts'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DataTable from '@/components/common/DataTable.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import { formatTimestamp } from '@/utils/format'
import { environmentStateLabel } from '@/utils/stateLabels'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import type { DataTableColumn } from '@/components/common/DataTable.vue'

const course = useCourseContext()
const courseId = course.courseId
const isContextMissing = computed(() => course.context.value === null)

const router = useRouter()

const state = ref<AsyncState<EnvironmentSummary[]>>({ kind: 'idle' })
let pollTimer: ReturnType<typeof setTimeout> | null = null

const columns: DataTableColumn<EnvironmentSummary & { actions?: never }>[] = [
  { key: 'displayLabel', title: '实验' },
  { key: 'observedState', title: '状态' },
  { key: 'runtimeKind', title: 'Runtime' },
  { key: 'eligibilityExpiresAt', title: '过期时间' },
  { key: 'actions', title: '操作' },
]

async function load() {
  const id = courseId.value
  if (!id) {
    state.value = {
      kind: 'blocked',
      diagnostic: makeDiagnostic('COURSE_CONTEXT_MISSING', '课程上下文缺失，无法加载实验列表。', false),
    }
    return
  }
  state.value = { kind: 'loading', message: '加载实验列表…' }
  const result = await listEnvironments({ query: { courseId: id } })
  if (result.error) {
    const problem = extractProblemDetails(result.error)
    state.value = {
      kind: 'error',
      diagnostic: makeDiagnostic(
        problem?.diagnosticCode ?? 'ENVIRONMENT_LIST_FAILED',
        problem?.detail ?? '加载实验列表失败',
        problem?.retryable ?? true,
      ),
    }
    return
  }
  const items = result.data.items ?? []
  state.value = items.length > 0 ? { kind: 'success', data: items } : { kind: 'empty' }
  scheduleRefresh()
}

/**
 * Non-terminal rows (requested/building/provisioning/…) refresh every 5s so
 * students see Ready arrive without a manual reload. Polling stops when the
 * page is hidden, the scope disposes, or every row reached a quiescent state.
 */
function scheduleRefresh() {
  if (pollTimer) {
    clearTimeout(pollTimer)
    pollTimer = null
  }
  if (state.value.kind !== 'success') return
  const transitioning = state.value.data.some((row) =>
    !['ready', 'stopped', 'failed', 'deleted', 'deleting', 'expiring'].includes(row.observedState),
  )
  if (!transitioning || document.visibilityState === 'hidden') return
  pollTimer = setTimeout(() => {
    pollTimer = null
    void load()
  }, 5000)
}

function onVisibilityChange() {
  if (document.visibilityState === 'visible') void load()
}

watch(courseId, load, { immediate: true })

if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', onVisibilityChange)
  onScopeDispose(() => {
    document.removeEventListener('visibilitychange', onVisibilityChange)
    if (pollTimer) clearTimeout(pollTimer)
  })
}

function openEnvironment(environmentId: string) {
  void router.push(`/student/environments?environmentId=${environmentId}`)
}
</script>

<style scoped>
.my-labs {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.labs-hint {
  margin: 8px 0 0;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}
</style>
