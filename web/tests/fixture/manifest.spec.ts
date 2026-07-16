import { describe, expect, it } from 'vitest'
import { computeManifestHash, fixtureManifest } from '@/fixture/manifest'
import { addSecondsIso, nowIso } from '@/fixture/utils/clock'
import { nextId, nextRevision, resetSequence } from '@/fixture/utils/sequence'

describe('fixture manifest', () => {
  it('has a stable hash across re-computation', async () => {
    const first = await computeManifestHash(fixtureManifest)
    const second = await computeManifestHash(fixtureManifest)
    expect(first).toBe(second)
    expect(first).toMatch(/^[0-9a-f]{16}$/)
  })

  it('records deterministic seed and epoch', () => {
    expect(fixtureManifest.seed).toBe(20260711)
    expect(fixtureManifest.epoch).toBe('2026-07-11T10:00:00.000Z')
  })
})

describe('fixture deterministic utilities', () => {
  it('clock is fixed', () => {
    expect(nowIso()).toBe('2026-07-11T10:00:00.000Z')
    expect(addSecondsIso(30)).toBe('2026-07-11T10:00:30.000Z')
  })

  it('sequence is reproducible after reset', () => {
    resetSequence()
    const run1 = [nextId('key'), nextRevision(), nextId('key')]

    resetSequence()
    const run2 = [nextId('key'), nextRevision(), nextId('key')]

    expect(run1).toEqual(run2)
    expect(run1).toEqual(['key-0001', 2, 'key-0003'])
  })
})
