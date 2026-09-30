import { describe, expect, it } from 'vitest'
import { validateVgpuImageImport } from '../e2e/support/vgpu-image-import.mjs'

const input = {
  binding: 'ubuntu-24.04-vgpu-v1',
  targetReference: 'registry.example.test/labweaver/ubuntu-vgpu:24.04',
  capacityBytes: 16 * 1024 ** 3,
}

const completion = {
  status: 202,
  body: { uploadId: '0197f0e0-0000-7000-8000-000000000030', revision: 1, state: 'queued' },
}

const status = {
  uploadId: completion.body.uploadId,
  revision: 2,
  state: 'imported',
  catalogId: '0197f0e0-0000-7000-8000-000000000031',
}

const entry = {
  catalogId: status.catalogId,
  kind: 'virtual_machine',
  binding: input.binding,
  sourceReference: input.targetReference,
  status: 'active',
  trustRevision: 1,
  capacityBytes: input.capacityBytes,
  format: 'qcow2',
  resolvedDigest: `sha256:${'a'.repeat(64)}`,
  diskSha256: 'b'.repeat(64),
  sizeBytes: 8 * 1024 ** 3,
}

describe('vGPU image import result validation', () => {
  it('accepts the numeric 202 completion response only after matching catalog readback', () => {
    expect(validateVgpuImageImport({ completion, status, entries: [entry], input })).toEqual({
      uploadId: status.uploadId,
      catalogId: status.catalogId,
      revision: status.revision,
    })
  })

  it('rejects non-202 completion responses without calling a response method on the number', () => {
    expect(() => validateVgpuImageImport({
      completion: { ...completion, status: 200 },
      status,
      entries: [entry],
      input,
    })).toThrow('LW_VGPU_IMAGE_COMPLETE_ACCEPTANCE_INVALID:200')
  })

  it('rejects an asynchronous import that terminates as failed', () => {
    expect(() => validateVgpuImageImport({
      completion,
      status: { ...status, state: 'failed', diagnostic: 'LW_PLATFORM_IMAGE_DISK_INVALID' },
      entries: [],
      input,
    })).toThrow('LW_VGPU_IMAGE_UPLOAD_TERMINAL_FAILURE:failed:LW_PLATFORM_IMAGE_DISK_INVALID')
  })

  it('rejects status that points to a catalog identity absent from the import catalog', () => {
    expect(() => validateVgpuImageImport({
      completion,
      status,
      entries: [],
      input,
    })).toThrow(`LW_VGPU_IMAGE_CATALOG_IDENTITY_MISSING:${status.catalogId}`)
  })

  it('rejects a catalog row whose imported image identity differs from the request', () => {
    expect(() => validateVgpuImageImport({
      completion,
      status,
      entries: [{ ...entry, binding: 'different-binding' }],
      input,
    })).toThrow(`LW_VGPU_IMAGE_IMPORTED_CATALOG_MISMATCH:${status.catalogId}`)
  })
})
