const UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB']

export function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B'
  const exp = Math.min(Math.floor(Math.log2(bytes) / 10), UNITS.length - 1)
  const value = bytes / 2 ** (exp * 10)
  return `${value.toFixed(exp === 0 ? 0 : 2)} ${UNITS[exp]}`
}

export function truncateSha256(sha256: string, head = 8, tail = 8): string {
  if (sha256.length <= head + tail + 3) return sha256
  return `${sha256.slice(0, head)}…${sha256.slice(-tail)}`
}

export function idempotencyKey(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) {
    return crypto.randomUUID()
  }
  return `${Date.now()}-${Math.random().toString(36).slice(2)}`
}

export function ifMatch(revision: number): string {
  return `rev-${revision}`
}

export function formatTimestamp(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  return d.toLocaleString('zh-CN', { dateStyle: 'short', timeStyle: 'medium' })
}
