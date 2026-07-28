/**
 * Deterministic identity generators for fixture mode.
 *
 * All outputs are derived from a fixed epoch and monotonic counters so that
 * screenshots, traces, and test assertions remain reproducible across runs.
 */

import { EPOCH } from './clock'

let uuidCounter = 0
let requestIdCounter = 0
let streamSequenceCounter = 0

export function resetIdentity(): void {
  uuidCounter = 0
  requestIdCounter = 0
  streamSequenceCounter = 0
}

function toHex(value: number, length: number): string {
  return value.toString(16).padStart(length, '0')
}

/**
 * Returns a deterministic UUIDv7-style identifier.
 *
 * If a prefix is provided, the result is `${prefix}-${hex(counter)}` so that
 * tests can read the fixture intent while still getting stable values.
 * Otherwise, a full 36-character UUIDv7-like string is produced from the
 * fixed epoch and an internal counter.
 */
export function nextUuid7(prefix?: string): string {
  uuidCounter += 1
  if (prefix) {
    return `${prefix}-${toHex(uuidCounter, 12)}`
  }

  const timestamp = EPOCH.getTime()
  const timeHex = timestamp.toString(16).padStart(12, '0')
  const rand = toHex(uuidCounter, 18)
  return `${timeHex.slice(0, 8)}-${timeHex.slice(8, 12)}-7${rand.slice(0, 3)}-8${rand.slice(3, 6)}-${rand.slice(6, 18)}`
}

/**
 * Returns the next deterministic stream sequence as a lower-case hex string.
 *
 * The public REST/SSE contract uses `StreamSequence` as a stable cursor, so
 * fixture sequences are padded to a fixed width for readable ordering.
 */
export function nextStreamSequence(): string {
  streamSequenceCounter += 1
  return streamSequenceCounter.toString(16).padStart(16, '0')
}

/**
 * Returns the next deterministic request correlation id.
 */
export function nextRequestId(): string {
  requestIdCounter += 1
  return `req-${toHex(requestIdCounter, 12)}`
}
