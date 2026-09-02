import type { EnvironmentManagementEventSchema, EnvironmentManagementStreamEvent } from '@/generated/contracts'
import { nextStreamSequence } from '../utils/identity'

interface EventLogEntry {
  sequence: string
  event: EnvironmentManagementEventSchema
}

const events: EventLogEntry[] = []

export function appendEvent(streamEvent: EnvironmentManagementStreamEvent): void {
  const sequence = nextStreamSequence()
  events.push({
    sequence,
    event: {
      cursor: sequence,
      data: streamEvent,
      event: streamEvent.data.kind,
    },
  })
}

export function getEvents(): readonly EventLogEntry[] {
  return events
}

export function resetEventLog(): void {
  events.length = 0
}

export interface SseResponseOptions {
  courseId: string
  after?: string
  lastEventId?: string
}

export function toSseResponse(options: SseResponseOptions): Response {
  const after = options.after ?? options.lastEventId ?? '0'
  const afterIndex = events.findIndex((e) => e.sequence > after)
  const slice = afterIndex >= 0 ? events.slice(afterIndex) : []
  const filtered = slice.filter((e) => e.event.data.courseId === options.courseId)

  const encoder = new TextEncoder()
  let index = 0
  const stream = new ReadableStream({
    pull(controller) {
      if (index >= filtered.length) {
        controller.close()
        return
      }
      const entry = filtered[index]
      index += 1
      const lines = [
        `id: ${entry.sequence}`,
        `event: ${entry.event.event}`,
        `data: ${JSON.stringify(entry.event)}`,
        '',
      ]
      controller.enqueue(encoder.encode(lines.join('\n')))
    },
  })

  return new Response(stream, {
    headers: {
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache',
      Connection: 'keep-alive',
    },
  })
}
