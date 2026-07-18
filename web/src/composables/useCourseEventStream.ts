import { ref, watch, type Ref } from 'vue'
import { createSseClient, type StreamEvent } from '@/generated/contracts/core/serverSentEvents.gen'
import type { TimelineEvent } from '@/components/common/EventTimeline.vue'
import { API_BASE_URL } from '@/config'

export type EventStreamState = 'idle' | 'connecting' | 'open' | 'closed' | 'error'

export function useCourseEventStream(courseId: Ref<string | undefined>, runId?: Ref<string | undefined>) {
  const events = ref<TimelineEvent[]>([])
  const state = ref<EventStreamState>('idle')
  const controller = new AbortController()

  function pushEvent(raw: StreamEvent<unknown>) {
    const data = typeof raw.data === 'object' && raw.data !== null ? (raw.data as Record<string, unknown>) : {}
    const eventRunId = data.runId ?? data.agentRunId ?? data.id
    // Only surface events that belong to this run when a run filter is supplied.
    if (runId?.value && eventRunId !== runId.value) return

    events.value.push({
      id: raw.id ?? `${Date.now()}-${events.value.length}`,
      title: (raw.event as string) || 'course-event',
      timestamp: new Date().toISOString(),
      description: JSON.stringify(raw.data),
    })
  }

  async function connect() {
    if (controller.signal.aborted) return
    const id = courseId.value
    if (!id) {
      state.value = 'closed'
      return
    }

    state.value = 'connecting'
    const token = localStorage.getItem('access_token')
    const url = `${window.location.origin}${API_BASE_URL}/events?courseId=${encodeURIComponent(id)}`
    const { stream } = createSseClient<unknown>({
      url,
      headers: token ? { Authorization: `Bearer ${token}` } : {},
      signal: controller.signal,
      sseMaxRetryAttempts: 3,
      onSseEvent: pushEvent,
      onSseError: (err) => {
        console.warn('[useCourseEventStream] SSE error', err)
        state.value = 'error'
      },
    })

    try {
      for await (const _event of stream) {
        void _event
        state.value = 'open'
      }
    } catch {
      state.value = 'error'
    } finally {
      if (!controller.signal.aborted) state.value = 'closed'
    }
  }

  function disconnect() {
    controller.abort()
    state.value = 'closed'
  }

  watch(courseId, () => {
    events.value = []
    if (courseId.value) connect()
    else disconnect()
  })

  return { events, state, connect, disconnect }
}
