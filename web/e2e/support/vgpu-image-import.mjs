const TRUST_REVISION = 1
const DISK_FORMAT = 'qcow2'

function formatBytes(bytes) {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  if (bytes === 0) return '0 B'
  const exponent = Math.min(Math.floor(Math.log2(bytes) / 10), units.length - 1)
  const value = bytes / 2 ** (exponent * 10)
  return `${value.toFixed(exponent === 0 ? 0 : 2)} ${units[exponent]}`
}

export function assertAcceptedVgpuImageCompletion(completion) {
  if (
    completion?.status !== 202
    || typeof completion.body?.uploadId !== 'string'
    || !Number.isInteger(completion.body?.revision)
    || !['queued', 'freezing', 'importing', 'imported'].includes(completion.body?.state)
  ) {
    throw new Error(`LW_VGPU_IMAGE_COMPLETE_ACCEPTANCE_INVALID:${completion?.status ?? 'missing'}`)
  }
  return completion.body
}

export function validateVgpuImageImport({ completion, status, entries, input }) {
  const body = assertAcceptedVgpuImageCompletion(completion)

  if (status?.state !== 'imported') {
    throw new Error(`LW_VGPU_IMAGE_UPLOAD_TERMINAL_FAILURE:${status?.state ?? 'missing'}:${status?.diagnostic ?? 'diagnostic-missing'}`)
  }
  if (
    status.uploadId !== body.uploadId
    || status.revision < body.revision
    || typeof status.catalogId !== 'string'
  ) {
    throw new Error('LW_VGPU_IMAGE_IMPORTED_STATUS_IDENTITY_MISMATCH')
  }

  if (!Array.isArray(entries)) throw new Error('LW_VGPU_IMAGE_CATALOG_API_INVALID')
  const importedEntries = entries.filter((entry) => entry.catalogId === status.catalogId)
  if (importedEntries.length !== 1) throw new Error(`LW_VGPU_IMAGE_CATALOG_IDENTITY_MISSING:${status.catalogId}`)
  const entry = importedEntries[0]
  if (
    entry.kind !== 'virtual_machine'
    || entry.binding !== input.binding
    || entry.sourceReference !== input.targetReference
    || entry.status !== 'active'
    || entry.trustRevision !== TRUST_REVISION
    || entry.capacityBytes !== input.capacityBytes
    || entry.format !== DISK_FORMAT
    || !/^sha256:[0-9a-f]{64}$/.test(entry.resolvedDigest)
    || !/^[0-9a-f]{64}$/.test(entry.diskSha256 ?? '')
    || !Number.isSafeInteger(entry.sizeBytes)
    || entry.sizeBytes < 1
  ) {
    throw new Error(`LW_VGPU_IMAGE_IMPORTED_CATALOG_MISMATCH:${status.catalogId}`)
  }
  return { uploadId: status.uploadId, catalogId: status.catalogId, revision: status.revision }
}

export function matchesVgpuImageCatalogRow(row, input) {
  return Boolean(
    row
      && row.kindLabel === '虚拟机'
      && row.binding === input.binding
      && row.sourceReference === input.targetReference
      && row.capacityLabel === formatBytes(input.capacityBytes)
      && row.format === DISK_FORMAT
      && row.statusLabel === '可用'
      && row.trustRevision === TRUST_REVISION,
  )
}
