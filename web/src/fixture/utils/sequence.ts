/**
 * Stable sequence generator for fixture IDs and revisions.
 */

import { resetIdentity } from './identity'

let counter = 0

export function resetSequence(): void {
  counter = 0
  resetIdentity()
}

export function nextId(prefix: string): string {
  counter += 1
  return `${prefix}-${counter.toString().padStart(4, '0')}`
}

export function nextRevision(): number {
  counter += 1
  return counter
}
