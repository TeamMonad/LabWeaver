import { describe, it, expect, vi, beforeEach } from 'vitest'
import { ref } from 'vue'
import { useCourseEventStream } from '@/composables/useCourseEventStream'
import { streamCourseEvents } from '@/generated/contracts'

type CapturedOptions = Record<string, unknown> & {
  onSseEvent?: (event: { id?: string; event?: string; data: unknown }) => void
  query: { courseId: string; after?: string }
}

let captured: CapturedOptions | null = null
let neverStreamAborted = false

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    streamCourseEvents: vi.fn(async (options: CapturedOptions) => {
      captured = options
      const signal = options.signal as AbortSignal
      async function* openStream(): AsyncGenerator<unknown> {
        await new Promise<void>((resolve) => {
          if (signal.aborted) resolve()
          else signal.addEventListener('abort', () => resolve(), { once: true })
        })
        neverStreamAborted = true
        yield 'closed' as unknown
      }
      return { stream: openStream() }
    }),
  }
})

function envelope(id: string, kind: string, payload: Record<string, unknown>) {
  return {
    id,
    event: kind,
    data: { cursor: id, event: kind, data: { kind, ...payload } },
  }
}

describe('useCourseEventStream', () => {
  beforeEach(() => {
    captured = null
    neverStreamAborted = false
    vi.mocked(streamCourseEvents).mockClear()
  })

  it('decodes the event envelope into readable timeline entries and dedupes by event id', async () => {
    const courseId = ref('course-101')
    const stream = useCourseEventStream(courseId)
    stream.connect()
    await vi.waitFor(() => expect(captured).not.toBeNull())

    captured!.onSseEvent!(envelope('1', 'environment_changed', { observedState: 'ready', revision: 3 }))
    captured!.onSseEvent!(envelope('1', 'environment_changed', { observedState: 'ready', revision: 3 }))
    captured!.onSseEvent!(envelope('2', 'operation_changed', { operationId: 'op-1234567890abcdef', state: 'running', revision: 4 }))

    await vi.waitFor(() => expect(stream.events.value.length).toBe(2))
    expect(stream.events.value[0].title).toBe('环境状态更新')
    expect(stream.events.value[0].description).toContain('运行中')
    expect(stream.events.value[1].title).toBe('环境操作更新')
    expect(stream.events.value[1].timestamp).toBeTruthy()
    stream.disconnect()
  })

  it('keeps course-scoped events without run identity and drops events of a different run', async () => {
    const courseId = ref('course-101')
    const runId = ref('run-1')
    const stream = useCourseEventStream(courseId, runId)
    stream.connect()
    await vi.waitFor(() => expect(captured).not.toBeNull())

    captured!.onSseEvent!(envelope('1', 'environment_changed', { observedState: 'building', revision: 2 }))
    captured!.onSseEvent!(envelope('2', 'agent_run_changed', { state: 'running', runId: 'run-2' }))
    captured!.onSseEvent!(envelope('3', 'agent_run_changed', { state: 'running', runId: 'run-1' }))

    await vi.waitFor(() => expect(stream.events.value.length).toBe(2))
    expect(stream.events.value[0].title).toBe('环境状态更新')
    expect(stream.events.value[1].title).toBe('AgentRun 更新')
    stream.disconnect()
  })

  it('resumes from the last seen event id after an explicit reconnect', async () => {
    const courseId = ref('course-101')
    const stream = useCourseEventStream(courseId)
    stream.connect()
    await vi.waitFor(() => expect(captured).not.toBeNull())

    captured!.onSseEvent!(envelope('7', 'access_grant_changed', { accessGrantId: 'g-1', state: 'active', revision: 2 }))
    await vi.waitFor(() => expect(stream.events.value.length).toBe(1))

    stream.disconnect()
    await vi.waitFor(() => expect(neverStreamAborted).toBe(true))
    stream.connect()
    await vi.waitFor(() => expect(vi.mocked(streamCourseEvents).mock.calls.length).toBe(2))
    const secondCall = vi.mocked(streamCourseEvents).mock.calls[1][0] as CapturedOptions
    expect(secondCall.query.after).toBe('7')
    stream.disconnect()
  })
})
