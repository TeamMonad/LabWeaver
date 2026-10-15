import { describe, expect, it } from 'vitest'
import type { PlatformConfigurationDesired } from '@/generated/contracts'
import {
  canonicalJson,
  diffConfigurations,
  formatSeconds,
  parseRequestYaml,
  parseSecondsList,
  parseSelector,
  requestToYaml,
} from '@/utils/platformConfiguration'

const base: PlatformConfigurationDesired = {
  lifecycle: { readOnlyRetentionSeconds: 604800, detailedLogRetentionSeconds: 2592000, expiryReminderSeconds: [86400, 3600], preemptionGraceSeconds: 60 },
  runtimePolicy: { defaultContainerUid: 0, allowedCapabilities: [], preauthorizations: [], privilegedAllowed: false, privilegedExecutionLocation: null },
  builderPolicy: { capabilities: [], allowPrivilegeEscalation: false, entitlements: [] },
}

describe('platform configuration utils', () => {
  it('round-trips the full update request through YAML', () => {
    const yaml = requestToYaml({ expectedRevision: 4, configuration: base })
    const parsed = parseRequestYaml(yaml)
    expect(parsed.ok).toBe(true)
    if (parsed.ok) {
      expect(parsed.request.expectedRevision).toBe(4)
      expect(canonicalJson(parsed.request.configuration)).toBe(canonicalJson(base))
    }
  })

  it('rejects duplicate keys, a missing revision and partial settings locally', () => {
    expect(parseRequestYaml('expectedRevision: 1\nexpectedRevision: 2\nconfiguration: {}\n')).toMatchObject({ ok: false, message: expect.stringContaining('重复键') })
    expect(parseRequestYaml('configuration:\n  lifecycle: {}\n')).toMatchObject({ ok: false, message: expect.stringContaining('expectedRevision') })
    expect(parseRequestYaml('expectedRevision: 1\nconfiguration:\n  lifecycle: {a: 1}\n')).toMatchObject({ ok: false, message: expect.stringContaining('runtimePolicy') })
  })

  it('lists field-level differences with arrays compared as a whole', () => {
    const changed = structuredClone(base)
    changed.lifecycle.preemptionGraceSeconds = 120
    changed.runtimePolicy.allowedCapabilities = ['SYS_PTRACE']
    expect(diffConfigurations(base, changed)).toEqual([
      { path: 'lifecycle.preemptionGraceSeconds', left: '60', right: '120' },
      { path: 'runtimePolicy.allowedCapabilities', left: '[]', right: '["SYS_PTRACE"]' },
    ])
    expect(diffConfigurations(base, structuredClone(base))).toEqual([])
  })

  it('formats and parses seconds and selectors', () => {
    expect(formatSeconds(604800)).toBe('7 天')
    expect(formatSeconds(3600)).toBe('1 小时')
    expect(formatSeconds(90)).toBe('90 秒')
    expect(parseSecondsList('86400, 3600')).toEqual([86400, 3600])
    expect(parseSecondsList('1, -2')).toBeNull()
    expect(parseSelector('pool=privileged\nzone = a')).toEqual({ pool: 'privileged', zone: 'a' })
    expect(parseSelector('broken')).toBeNull()
  })
})
