import { describe, expect, it } from 'vitest'
import { computeDiff } from '@/composables/useCandidateApproval'
import { evaluateImageGate } from '@/types/candidate'
import type { ImageArtifact, ImagePolicyEvaluation } from '@/generated/contracts'

function makeArtifact(overrides: Partial<ImageArtifact> = {}): ImageArtifact {
  return {
    kind: 'container',
    id: 'image-1',
    build_request_id: 'build-1',
    repository: 'registry.labweaver.local/candidate-1',
    digest: 'sha256:image',
    ...overrides,
  }
}

function makeEvaluation(overrides: Partial<ImagePolicyEvaluation> = {}): ImagePolicyEvaluation {
  return {
    artifactId: 'image-1',
    artifactSha256: 'sha256:image',
    evaluatedAt: '2026-07-16T08:00:00.000Z',
    maxEvidenceAgeMilliseconds: 3600000,
    passed: true,
    policyId: 'policy-1',
    policyRevision: 1,
    scannerDatabaseSha256: 'sha256:scanner-db',
    scannerName: 'trivy',
    scannerVersion: '1.0.0',
    validUntil: '2026-07-16T10:00:00.000Z',
    vulnerabilities: { critical: 0, high: 0, medium: 0, low: 0, unknown: 0 },
    ...overrides,
  }
}

describe('computeDiff', () => {
  it('marks added, removed, and modified fields', () => {
    const changes = computeDiff(
      { a: 1, b: 2 },
      { a: 1, b: 3, c: 4 },
    )
    expect(changes).toContainEqual({ field: 'a', before: '1', after: '1', kind: 'unchanged' })
    expect(changes).toContainEqual({ field: 'b', before: '2', after: '3', kind: 'modified' })
    expect(changes).toContainEqual({ field: 'c', after: '4', kind: 'added' })
  })

  it('does not emit an empty field row for a null baseline', () => {
    const changes = computeDiff(null, { name: 'x' })
    expect(changes).not.toContainEqual(expect.objectContaining({ field: '' }))
    expect(changes).toContainEqual({ field: 'name', after: 'x', kind: 'added' })
  })
})

describe('evaluateImageGate', () => {
  it('passes when evidence matches', () => {
    const result = evaluateImageGate(makeArtifact(), makeEvaluation())
    expect(result.status).toBe('passed')
  })

  it('warns on high vulnerabilities', () => {
    const result = evaluateImageGate(makeArtifact(), makeEvaluation({ vulnerabilities: { critical: 0, high: 2, medium: 0, low: 0, unknown: 0 } }))
    expect(result.status).toBe('warning')
  })

  it('blocks on critical vulnerabilities', () => {
    const result = evaluateImageGate(makeArtifact(), makeEvaluation({ vulnerabilities: { critical: 1, high: 0, medium: 0, low: 0, unknown: 0 } }))
    expect(result.status).toBe('blocked')
  })

  it('blocks when the scanner gate fails', () => {
    const result = evaluateImageGate(makeArtifact(), makeEvaluation({ passed: false }))
    expect(result.status).toBe('blocked')
  })

  it('blocks on digest mismatch', () => {
    const result = evaluateImageGate(makeArtifact({ digest: 'sha256:other' }), makeEvaluation())
    expect(result.status).toBe('blocked')
  })

  it('blocks when evidence is missing', () => {
    expect(evaluateImageGate(undefined, undefined).status).toBe('blocked')
  })
})
