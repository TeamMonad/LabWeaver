<template>
  <section class="candidate-build-task" :aria-label="label">
    <p role="status">{{ label }}：{{ stateLabel }}</p>
    <p v-if="task?.status.cancellationRequested && !terminal">取消已请求，正在停止构建并回收本次临时资源。</p>
    <p v-if="terminal && task?.status.cleanupVerified === false">构建已结束，临时资源仍在回收中；尚未确认回收完成。</p>
    <p v-if="task?.status.cleanupVerified === true">本次构建临时资源已回收。</p>
    <DiagnosticBanner v-if="diagnostic" :code="diagnostic.code" :message="diagnostic.message"
      :retryable="diagnostic.retryable" @retry="refresh" />
    <DiagnosticBanner v-if="cancelDiagnostic" :code="cancelDiagnostic.code" :message="cancelDiagnostic.message" :retryable="false" />
    <DiagnosticBanner v-if="task?.status.diagnosticCode" :code="task.status.diagnosticCode"
      :message="`${label}${stateLabel}`" :retryable="false" />
    <button v-if="canCancel" type="button" class="outlined-button danger-button" :disabled="acting"
      @click="cancel">{{ acting ? '提交取消中…' : `取消${label}` }}</button>
    <button v-if="permitted && cancelRetryable" type="button" class="outlined-button" :disabled="acting"
      @click="cancel">重新提交同一次取消请求</button>
  </section>
</template>

<script setup lang="ts">
import { computed, onScopeDispose, ref, watch } from 'vue'
import { cancelProjectCandidateBuild, getProjectCandidateBuild } from '@/generated/contracts'
import type { CandidateBuildTaskSchema } from '@/generated/contracts'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import { extractProblemDetails, makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'
import { useAuth } from '@/composables/useAuth'
import { hasAnyRole, PLATFORM_ROLES, rolesFromProfile } from '@/utils/navigation'

const props = defineProps<{ projectId: string; candidateId: string; target: CandidateBuildTaskSchema['target'] }>()
const auth = useAuth()
const permitted = computed(() => auth.isAuthenticated.value && hasAnyRole(rolesFromProfile(auth.user.value?.profile), PLATFORM_ROLES))
const task = ref<CandidateBuildTaskSchema | null>(null)
const diagnostic = ref<DiagnosticViewModel | null>(null)
const cancelDiagnostic = ref<DiagnosticViewModel | null>(null)
const acting = ref(false)
const cancelRetryable = ref(false)
let generation = 0
let readGeneration = 0
let timer: ReturnType<typeof setTimeout> | null = null
let cancellation: { task: CandidateBuildTaskSchema; key: string } | null = null
const label = computed(() => props.target === 'environment' ? '环境镜像构建' : '评测镜像构建')
const terminal = computed(() => Boolean(task.value && ['succeeded', 'failed', 'cancelled'].includes(task.value.status.state)))
const canCancel = computed(() => Boolean(permitted.value && task.value && !terminal.value && !task.value.status.cancellationRequested && !cancelRetryable.value))
const stateLabel = computed(() => {
  const status = task.value?.status
  if (!status) return '尚未开始，无可取消任务'
  if (!terminal.value && status.cancellationRequested) return '取消中'
  return { requested: '等待构建', running: '构建中', succeeded: '已完成', failed: '失败', cancelled: '已取消' }[status.state]
})

function stopPolling() {
  if (timer) clearTimeout(timer)
  timer = null
}

async function refresh() {
  stopPolling()
  const current = generation
  const read = ++readGeneration
  const path = { projectId: props.projectId, candidateId: props.candidateId, target: props.target }
  const result = await getProjectCandidateBuild({ path })
  if (current !== generation || read !== readGeneration) return
  if (result.error || !result.data) {
    const problem = extractProblemDetails(result.error)
    if (result.response?.status === 404) {
      task.value = null
      cancellation = null
      cancelRetryable.value = false
    }
    diagnostic.value = result.response?.status === 404 ? null : makeDiagnostic(problem?.diagnosticCode ?? 'CANDIDATE_BUILD_STATUS_FAILED', problem?.detail ?? '读取构建状态失败', true)
  } else {
    if (result.data.candidateId !== props.candidateId || result.data.target !== props.target || result.data.status.projectId !== props.projectId) {
      task.value = null
      diagnostic.value = makeDiagnostic('CANDIDATE_BUILD_STALE_CONTEXT', '构建任务引用已变化，请重新打开当前候选。', false)
      return
    }
    if (!task.value || result.data.status.revision >= task.value.status.revision) task.value = result.data
    diagnostic.value = null
    if (task.value?.status.cancellationRequested || terminal.value) {
      cancellation = null
      cancelRetryable.value = false
      cancelDiagnostic.value = null
    }
  }
  if (!terminal.value || task.value?.status.cleanupVerified === false) {
    timer = setTimeout(() => void refresh(), 3000)
  }
}

async function cancel() {
  if (!permitted.value || acting.value || !task.value || (!canCancel.value && !cancelRetryable.value)) return
  cancellation ??= { task: task.value, key: idempotencyKey() }
  const current = generation
  const selected = cancellation
  if (selected.task.candidateId !== props.candidateId || selected.task.target !== props.target || selected.task.status.projectId !== props.projectId) return
  acting.value = true
  readGeneration += 1
  stopPolling()
  cancelDiagnostic.value = null
  const result = await cancelProjectCandidateBuild({
    path: { projectId: selected.task.status.projectId, candidateId: selected.task.candidateId, target: selected.task.target },
    headers: { 'Idempotency-Key': selected.key, 'If-Match': ifMatch(selected.task.status.revision) },
    body: { buildRequestId: selected.task.status.buildRequestId, expectedState: selected.task.status.state, expectedRevision: selected.task.status.revision },
  })
  if (current !== generation) return
  acting.value = false
  if (result.error || !result.data) {
    const problem = extractProblemDetails(result.error)
    const status = result.response?.status
    cancelRetryable.value = problem?.retryable ?? (status === undefined || status === 408 || status === 429 || status >= 500)
    if (status !== undefined && [400, 401, 403, 404, 409, 410, 422].includes(status)) cancelRetryable.value = false
    cancelDiagnostic.value = makeDiagnostic(problem?.diagnosticCode ?? 'CANDIDATE_BUILD_CANCEL_FAILED', problem?.detail ?? '取消构建请求失败', cancelRetryable.value)
    if (!cancelRetryable.value) cancellation = null
  } else {
    if (result.data.candidateId !== selected.task.candidateId || result.data.target !== selected.task.target
      || result.data.status.projectId !== selected.task.status.projectId || result.data.status.buildRequestId !== selected.task.status.buildRequestId) {
      cancelDiagnostic.value = makeDiagnostic('CANDIDATE_BUILD_STALE_CONTEXT', '取消结果引用已变化，请重新打开当前候选。', false)
      cancellation = null
      cancelRetryable.value = false
      await refresh()
      return
    }
    task.value = result.data
    cancelDiagnostic.value = null
    cancelRetryable.value = false
    cancellation = null
  }
  await refresh()
}

watch(() => [props.projectId, props.candidateId, props.target], () => {
  generation += 1
  stopPolling()
  task.value = null
  diagnostic.value = null
  cancelDiagnostic.value = null
  acting.value = false
  cancelRetryable.value = false
  cancellation = null
  void refresh()
}, { immediate: true })
onScopeDispose(() => { generation += 1; stopPolling() })
</script>
