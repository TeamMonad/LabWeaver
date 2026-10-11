import { describe, expect, it } from 'vitest'
import { validateConfigProbeBaseDiskCatalogEntry } from '../e2e/support/config-probe-catalog.mjs'

const baseDisk = {
  binding: 'lw127-vgpu-bootfix-20261009',
  sourceRegistryDigest: 'docker://harbor.lab.lan/labweaver-system/lw127-vgpu-bootfix-20261009@sha256:de1cde9417477346b50c81e3aa29af1b27e73e6539584db32d7a84042c58604a',
  capacityBytes: 16 * 1024 ** 3,
}

const registryEntry = {
  kind: 'virtual_machine',
  binding: baseDisk.binding,
  sourceReference: 'harbor.lab.lan/labweaver-system/lw127-vgpu-bootfix-20261009:latest',
  resolvedDigest: 'sha256:de1cde9417477346b50c81e3aa29af1b27e73e6539584db32d7a84042c58604a',
  status: 'active',
  trustRevision: 1,
  format: null,
  capacityBytes: null,
  diskSha256: null,
}

describe('ConfigProbe VM catalog validation', () => {
  it('accepts a reviewed registry inventory without a disk descriptor', () => {
    expect(validateConfigProbeBaseDiskCatalogEntry(registryEntry, baseDisk)).toBe(true)
  })

  it('accepts omitted descriptor fields from the serialized registry inventory', () => {
    const serializedEntry = { ...registryEntry }
    delete serializedEntry.format
    delete serializedEntry.capacityBytes
    delete serializedEntry.diskSha256
    expect(validateConfigProbeBaseDiskCatalogEntry(serializedEntry, baseDisk)).toBe(true)
  })

  it('accepts a complete imported disk descriptor when it matches the reviewed base', () => {
    expect(validateConfigProbeBaseDiskCatalogEntry({
      ...registryEntry,
      format: 'qcow2',
      capacityBytes: baseDisk.capacityBytes,
      diskSha256: 'a'.repeat(64),
    }, baseDisk)).toBe(true)
  })

  it('rejects a partial disk descriptor', () => {
    expect(() => validateConfigProbeBaseDiskCatalogEntry({
      ...registryEntry,
      format: 'qcow2',
    }, baseDisk)).toThrow('CONFIG_PROBE_CATALOG_DESCRIPTOR_PARTIAL')
  })

  it('rejects an inventory with a mismatched trust revision', () => {
    expect(() => validateConfigProbeBaseDiskCatalogEntry({ ...registryEntry, trustRevision: 2 }, baseDisk))
      .toThrow('CONFIG_PROBE_CATALOG_IDENTITY_INVALID')
  })
})
