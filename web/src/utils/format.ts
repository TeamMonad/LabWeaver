const UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB']

export function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B'
  const exp = Math.min(Math.floor(Math.log2(bytes) / 10), UNITS.length - 1)
  const value = bytes / 2 ** (exp * 10)
  return `${value.toFixed(exp === 0 ? 0 : 2)} ${UNITS[exp]}`
}

export function truncateSha256(sha256: string, head = 8, tail = 8): string {
  if (sha256.length <= head + tail + 1) return sha256
  return `${sha256.slice(0, head)}…${sha256.slice(-tail)}`
}

/** Renders a stable short form of a long identifier for dense UI surfaces. */
export function shortId(id: string, head = 12): string {
  return id.length <= head ? id : `${id.slice(0, head)}…`
}

export function idempotencyKey(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) {
    return crypto.randomUUID()
  }
  return `${Date.now()}-${Math.random().toString(36).slice(2)}`
}

/**
 * Build the If-Match header value for a revision.
 *
 * The Public API contract uses strong ETags: the backend parses the quoted
 * `"rev-<n>"` form via `StrongEtag::parse` (crates/contracts/src/http.rs) and
 * rejects unquoted or weak validators.
 */
export function ifMatch(revision: number): string {
  return `"rev-${revision}"`
}

export function formatTimestamp(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  return d.toLocaleString('zh-CN', { dateStyle: 'short', timeStyle: 'medium' })
}
