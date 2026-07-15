/**
 * 稳定的序列生成器，用于 fixture 中的 ID、revision 等。
 */

let counter = 0

export function resetSequence(): void {
  counter = 0
}

export function nextId(prefix: string): string {
  counter += 1
  return `${prefix}-${counter.toString().padStart(4, '0')}`
}

export function nextRevision(): number {
  counter += 1
  return counter
}
