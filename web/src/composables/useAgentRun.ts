import { reactive, ref, type Ref } from 'vue'
import { createAgentRun, getAgentRun, cancelAgentRun, retryAgentRunTrack } from '@/generated/contracts'
import type { AgentRunSchema, CreateAgentRunRequestSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

const TERMINAL_STATES = new Set(['succeeded', 'failed', 'cancelled', 'partially_succeeded'])

export function useAgentRun(courseId: Ref<string | undefined>) {
  const run = ref<AsyncState<AgentRunSchema>>({ kind: 'idle' })
  const polling = ref(false)
  let pollTimer: ReturnType<typeof setTimeout> | null = null

  async function start(request: CreateAgentRunRequestSchema) {
    const id = courseId.value
    if (!id) {
      run.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic('COURSE_CONTEXT_MISSING', '课程上下文缺失，无法启动 AgentRun。', false),
      }
      return
    }

    run.value = { kind: 'loading', message: '创建 AgentRun…' }
    const result = await createAgentRun({
      path: { courseId: id },
      headers: { 'Idempotency-Key': idempotencyKey() },
      body: request,
    })

    if (result.error) {
      const problem = extractProblemDetails(result.error)
      run.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'AGENT_RUN_CREATE_FAILED',
          problem?.detail ?? '创建 AgentRun 失败',
          problem?.retryable ?? true,
        ),
      }
      return
    }

    run.value = { kind: 'success', data: result.data }
    beginPolling(result.data.id)
  }

  async function poll(runId: string) {
    const id = courseId.value
    if (!id) return
    const result = await getAgentRun({ path: { courseId: id, runId } })
    if (result.error) {
      // Surface polling errors as non-fatal so the user can still see the last known state.
      console.warn('[useAgentRun] poll failed', result.error)
      return
    }
    run.value = { kind: 'success', data: result.data }
    if (!TERMINAL_STATES.has(result.data.state)) {
      pollTimer = setTimeout(() => poll(runId), 3000)
    } else {
      polling.value = false
    }
  }

  function beginPolling(runId: string) {
    polling.value = true
    if (pollTimer) clearTimeout(pollTimer)
    poll(runId)
  }

  function stopPolling() {
    polling.value = false
    if (pollTimer) {
      clearTimeout(pollTimer)
      pollTimer = null
    }
  }

  async function cancel() {
    const current = run.value.kind === 'success' ? run.value.data : undefined
    const id = courseId.value
    if (!current || !id) return
    const result = await cancelAgentRun({
      path: { courseId: id, runId: current.id },
      headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(current.revision) },
    })
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      run.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'AGENT_RUN_CANCEL_FAILED',
          problem?.detail ?? '取消 AgentRun 失败',
          problem?.retryable ?? true,
        ),
      }
      return
    }
    // The server accepted the cancel command; poll once to reflect the new state quickly.
    await poll(current.id)
  }

  async function retryTrack(track: 'environment' | 'evaluation') {
    const current = run.value.kind === 'success' ? run.value.data : undefined
    const id = courseId.value
    if (!current || !id) return
    const result = await retryAgentRunTrack({
      path: { courseId: id, runId: current.id, track },
      headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(current.revision) },
    })
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      run.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'AGENT_RUN_RETRY_FAILED',
          problem?.detail ?? `重试 ${track} 轨道失败`,
          problem?.retryable ?? true,
        ),
      }
      return
    }
    beginPolling(current.id)
  }

  return reactive({ run, polling, start, cancel, retryTrack, stopPolling, beginPolling })
}
