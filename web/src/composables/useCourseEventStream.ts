import { ref, watch, type Ref } from 'vue'
import { streamCourseEvents } from '@/generated/contracts'
import type { StreamEvent } from '@/generated/contracts/core/serverSentEvents.gen'
import type { TimelineEvent } from '@/components/common/EventTimeline.vue'
import { environmentStateLabel } from '@/utils/stateLabels'
import { shortId } from '@/utils/format'
import { extractProblemDetails, makeDiagnostic, type DiagnosticViewModel } from '@/types/async'

export type EventStreamState = 'idle' | 'connecting' | 'open' | 'closed' | 'error'

/** The course event stream carries a management subset today; agent-domain
 * events are a registered contract gap (#178). Decoding is defensive so new
 * event kinds stay visible instead of being silently dropped. */
function decodePayload(raw: StreamEvent<unknown>): Record<string, unknown> | null {
  // Wire envelope: {cursor, event, data: {kind, ...payload}} (RFC via contract
  // EnvironmentManagementEventSchema). Be defensive against flat payloads.
  const envelope = raw.data as { data?: unknown } | null
  if (envelope && typeof envelope === 'object' && envelope.data && typeof envelope.data === 'object') {
    return envelope.data as Record<string, unknown>
  }
  if (raw.data && typeof raw.data === 'object') {
    return raw.data as Record<string, unknown>
  }
  return null
}

function describeEvent(kind: string, payload: Record<string, unknown>): { title: string; description: string } {
  const state = typeof payload.state === 'string' ? payload.state : undefined
  switch (kind) {
    case 'environment_changed': {
      const observed = typeof payload.observedState === 'string' ? payload.observedState : undefined
      return {
        title: '环境状态更新',
        description: [
          observed ? `状态：${environmentStateLabel(observed)}` : undefined,
          typeof payload.revision === 'number' ? `修订 rev-${payload.revision}` : undefined,
        ]
          .filter(Boolean)
          .join(' · '),
      }
    }
    case 'operation_changed':
      return {
        title: '环境操作更新',
        description: [
          typeof payload.operationId === 'string' ? `操作 ${shortId(payload.operationId, 8)}` : undefined,
          state ? `状态：${state}` : undefined,
          typeof payload.revision === 'number' ? `修订 rev-${payload.revision}` : undefined,
        ]
          .filter(Boolean)
          .join(' · '),
      }
    case 'access_grant_changed':
      return {
        title: '访问授权更新',
        description: [
          typeof payload.accessGrantId === 'string' ? `授权 ${shortId(payload.accessGrantId, 8)}` : undefined,
          state ? `状态：${state}` : undefined,
        ]
          .filter(Boolean)
          .join(' · '),
      }
    case 'agent_run_changed':
      return {
        title: 'AgentRun 更新',
        description: [state ? `状态：${state}` : undefined].filter(Boolean).join(' · '),
      }
    default: {
      const details = Object.entries(payload)
        .filter(([key]) => key !== 'kind')
        .slice(0, 4)
        .map(([key, value]) => `${key}=${typeof value === 'string' ? shortId(value, 24) : String(value)}`)
      return { title: `事件：${kind}`, description: details.join(' · ') || '（无附加字段）' }
    }
  }
}

export function useCourseEventStream(courseId: Ref<string | undefined>, runId?: Ref<string | undefined>) {
  const events = ref<TimelineEvent[]>([])
  const state = ref<EventStreamState>('idle')
  const errorDiagnostic = ref<DiagnosticViewModel | null>(null)
  let controller: AbortController | null = null
  let generation = 0
  const seenEventIds = new Set<string>()
  let lastEventId: string | undefined

  function pushEvent(raw: StreamEvent<unknown>) {
    if (typeof raw.id === 'string' && raw.id) {
      if (seenEventIds.has(raw.id)) return
      seenEventIds.add(raw.id)
      lastEventId = raw.id
    }
    const payload = decodePayload(raw)
    if (!payload) return
    const kind = (raw.event as string) || (typeof payload.kind === 'string' ? payload.kind : 'course-event')
    const decoded = describeEvent(kind, payload)
    const eventRunId =
      typeof payload.runId === 'string'
        ? payload.runId
        : typeof payload.agentRunId === 'string'
          ? payload.agentRunId
          : undefined
    // Drop only when the event explicitly belongs to a different run; events
    // without run identity are course-scoped context and stay visible.
    if (runId?.value && eventRunId !== undefined && eventRunId !== runId.value) return

    events.value.push({
      id: raw.id ?? `local-${seenEventIds.size}-${events.value.length}`,
      title: decoded.title,
      timestamp: typeof payload.effectiveAt === 'string' ? payload.effectiveAt : new Date().toISOString(),
      description: decoded.description,
    })
  }

  async function connect() {
    const id = courseId.value
    if (!id) {
      state.value = 'closed'
      return
    }
    // Guard against concurrent streams: a course change or explicit reconnect
    // must not leave two generators appending events.
    generation += 1
    const currentGeneration = generation
    controller?.abort()
    controller = new AbortController()
    // Capturing fetch lets us decode RFC 9457 ProblemDetails from a failed SSE
    // handshake instead of showing a bare "SSE failed: 401" message.
    const problemFetch: typeof fetch = async (input, init) => {
      const response = await globalThis.fetch(input as RequestInfo, init)
      if (!response.ok) {
        try {
          const body = await response.clone().json()
          const problem = extractProblemDetails(body)
          if (problem) {
            errorDiagnostic.value = makeDiagnostic(
              problem.diagnosticCode ?? 'EVENT_STREAM_FAILED',
              problem.detail ?? '课程事件流连接失败',
              problem.retryable ?? false,
            )
          }
        } catch {
          // not a ProblemDetails body; fall through to the generic handler
        }
      }
      return response
    }

    state.value = 'connecting'
    try {
      const result = await streamCourseEvents({
        query: { courseId: id, ...(lastEventId !== undefined ? { after: lastEventId } : {}) },
        signal: controller.signal,
        fetch: problemFetch,
        sseMaxRetryAttempts: 3,
        onSseEvent: pushEvent,
        onSseError: () => {
          state.value = 'error'
        },
      })
      for await (const _event of result.stream) {
        void _event
        if (currentGeneration !== generation) break
        if (state.value !== 'error') state.value = 'open'
      }
      if (currentGeneration === generation && !controller.signal.aborted) state.value = 'closed'
    } catch (err) {
      if (currentGeneration !== generation || controller.signal.aborted) return
      if (!errorDiagnostic.value) {
        const problem = extractProblemDetails(err)
        errorDiagnostic.value = makeDiagnostic(
          problem?.diagnosticCode ?? 'EVENT_STREAM_FAILED',
          problem?.detail ?? '课程事件流不可用；已停止自动重连。',
          problem?.retryable ?? true,
        )
      }
      state.value = 'error'
    }
  }

  function disconnect() {
    generation += 1
    controller?.abort()
    controller = null
    state.value = 'closed'
  }

  function clearError() {
    errorDiagnostic.value = null
  }

  watch(courseId, () => {
    events.value = []
    seenEventIds.clear()
    errorDiagnostic.value = null
    if (courseId.value) void connect()
    else disconnect()
  })

  return { events, state, errorDiagnostic, connect, disconnect, clearError }
}
