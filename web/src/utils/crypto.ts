/**
 * Browser-side SHA-256 helpers for package manifest and file hashing.
 *
 * These utilities use the Web Crypto API and produce lowercase hex digests
 * matching the LabWeaver v1 contract (`Sha256Digest`).
 */

export async function sha256Hex(input: ArrayBuffer | Uint8Array): Promise<string> {
  const buffer = input instanceof Uint8Array ? input.buffer : input
  const digest = await crypto.subtle.digest('SHA-256', buffer)
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

export async function sha256File(file: File): Promise<string> {
  const buffer = await file.arrayBuffer()
  return sha256Hex(buffer)
}

/**
 * Canonical JSON serialization used for the package manifest digest.
 *
 * Object keys are sorted recursively. Arrays keep caller-provided order;
 * callers should sort array entries (e.g. files by path) before hashing.
 */
export function canonicalJson(value: unknown): string {
  return JSON.stringify(sortValue(value))
}

function sortValue(value: unknown): unknown {
  if (value === null || typeof value !== 'object') return value
  if (Array.isArray(value)) return value.map(sortValue)
  const sorted: Record<string, unknown> = {}
  for (const key of Object.keys(value as Record<string, unknown>).sort()) {
    sorted[key] = sortValue((value as Record<string, unknown>)[key])
  }
  return sorted
}

export function computeManifestSha256<T extends { path: string }>(files: T[]): Promise<string> {
  const sorted = [...files].sort((a, b) => a.path.localeCompare(b.path))
  return sha256Hex(new TextEncoder().encode(canonicalJson(sorted)))
}
