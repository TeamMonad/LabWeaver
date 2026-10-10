const SHA256_HEX = /^[0-9a-f]{64}$/

function canonicalRegistryParts(value) {
  if (typeof value !== 'string') return null
  const withoutScheme = value.replace(/^docker:\/\//, '')
  const at = withoutScheme.lastIndexOf('@')
  if (at <= 0 || at === withoutScheme.length - 1) return null
  const repository = withoutScheme.slice(0, at)
  const digest = withoutScheme.slice(at + 1)
  if (!repository || !/^sha256:[0-9a-f]{64}$/.test(digest)) return null
  return { repository, digest }
}

function registryRepository(value) {
  if (typeof value !== 'string' || value.trim() === '') return null
  const withoutScheme = value.replace(/^docker:\/\//, '')
  const reference = withoutScheme.split('@', 1)[0]
  if (!reference) return null
  const lastSlash = reference.lastIndexOf('/')
  const lastColon = reference.lastIndexOf(':')
  return lastColon > lastSlash ? reference.slice(0, lastColon) : reference
}

/**
 * Validates the deployment-reviewed VM identity used by the ConfigProbe journey.
 *
 * Registry inventory entries intentionally omit the VM disk descriptor. An imported archive may
 * carry the complete descriptor instead; a partially populated descriptor is invalid in either
 * case and must never be treated as a reviewed base disk.
 */
export function validateConfigProbeBaseDiskCatalogEntry(entry, baseDisk, trustRevision = 1) {
  if (!entry || typeof entry !== 'object' || !baseDisk || typeof baseDisk !== 'object') {
    throw new Error('CONFIG_PROBE_CATALOG_ENTRY_INVALID')
  }
  if (
    entry.kind !== 'virtual_machine'
    || entry.binding !== baseDisk.binding
    || entry.status !== 'active'
    || entry.trustRevision !== trustRevision
  ) {
    throw new Error('CONFIG_PROBE_CATALOG_IDENTITY_INVALID')
  }
  const expected = canonicalRegistryParts(baseDisk.sourceRegistryDigest)
  const actualDigest = typeof entry.resolvedDigest === 'string' ? entry.resolvedDigest : ''
  if (
    !expected
    || registryRepository(entry.sourceReference) !== expected.repository
    || actualDigest !== expected.digest
  ) {
    throw new Error('CONFIG_PROBE_CATALOG_REGISTRY_IDENTITY_INVALID')
  }
  if (!Number.isSafeInteger(baseDisk.capacityBytes) || baseDisk.capacityBytes < 1) {
    throw new Error('CONFIG_PROBE_BASE_DISK_CAPACITY_INVALID')
  }

  const descriptor = [entry.format, entry.capacityBytes, entry.diskSha256]
  const absent = descriptor.every((value) => value == null)
  const complete = entry.format != null && entry.capacityBytes != null && entry.diskSha256 != null
  if (!absent && !complete) throw new Error('CONFIG_PROBE_CATALOG_DESCRIPTOR_PARTIAL')

  if (complete && (
    entry.format !== 'qcow2'
    || entry.capacityBytes !== baseDisk.capacityBytes
    || typeof entry.diskSha256 !== 'string'
    || !SHA256_HEX.test(entry.diskSha256)
  )) {
    throw new Error('CONFIG_PROBE_CATALOG_DESCRIPTOR_INVALID')
  }
  return true
}
