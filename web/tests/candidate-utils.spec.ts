import { describe, expect, it } from 'vitest'
import { computeDiff } from '@/composables/useCandidateApproval'
import { evaluateImageGate } from '@/types/candidate'
import type { ImageArtifact, ImagePolicyEvaluation } from '@/generated/contracts'

function makeSignature(overrides: Partial<ImageArtifact['signature']> = {}) {
  return {
    certificateSha256: 'sha256:cert',
    certificateSubject: 'spiffe://labweaver/image-builder',
    ctLogId: 'ct',
    fulcioIssuer: 'https://fixture.fulcio.dev',
    rekorInclusionProofSha256: 'sha256:proof',
    rekorLogId: 'rekor',
    rekorLogIndex: 1,
    sctSha256: 'sha256:sct',
    signatureSha256: 'sha256:sig',
    subjectDigest: 'sha256:image',
    trustBundleSha256: 'sha256:trust',
    verifiedAt: '2026-07-16T08:00:00.000Z',
    ...overrides,
  }
}

function makeArtifact(overrides: Partial<ImageArtifact> = {}): ImageArtifact {
  return {
    kind: 'container',
    id: 'image-1',
    build_request_id: 'build-1',
    repository: 'registry.labweaver.local/candidate-1',
    immutable_tag: 'release-1',
    digest: 'sha256:image',
    provenance: {
      artifactId: 'artifact-1',
      mediaType: 'application/vnd.in-toto+json',
      objectVersion: 'v1',
      sha256: 'sha256:provenance',
      sizeBytes: 1,
      storeBinding: 'store',
    },
    sbom: {
      artifactId: 'artifact-2',
      mediaType: 'application/spdx+json',
      objectVersion: 'v1',
      sha256: 'sha256:sbom',
      sizeBytes: 1,
      storeBinding: 'store',
    },
    signature: makeSignature(),
    ...overrides,
  }
}

function makeEvaluation(overrides: Partial<ImagePolicyEvaluation> = {}): ImagePolicyEvaluation {
  return {
    artifactId: 'image-1',
    artifactSha256: 'sha256:image',
    evaluatedAt: '2026-07-16T08:00:00.000Z',
    expectedCertificateSubject: 'spiffe://labweaver/image-builder',
    expectedFulcioIssuer: 'https://fixture.fulcio.dev',
    maxEvidenceAgeMilliseconds: 3600000,
    passed: true,
    policyId: 'policy-1',
    policyRevision: 1,
    requireCtSct: true,
    requireRekorInclusion: true,
    scannerDatabaseSha256: 'sha256:scanner-db',
    scannerName: 'trivy',
    scannerVersion: '1.0.0',
    trustBundleSha256: 'sha256:trust',
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

  it('blocks on unsigned artifact', () => {
    const artifact = makeArtifact() as { signature?: unknown }
    delete artifact.signature
    const result = evaluateImageGate(artifact as ImageArtifact, makeEvaluation())
    expect(result.status).toBe('blocked')
    expect(result.reasons.join(' ')).toContain('签名')
  })

  it('blocks on wrong issuer', () => {
    const result = evaluateImageGate(makeArtifact({ signature: makeSignature({ fulcioIssuer: 'https://wrong.example.com' }) }), makeEvaluation())
    expect(result.status).toBe('blocked')
  })

  it('blocks on digest mismatch', () => {
    const result = evaluateImageGate(makeArtifact({ signature: makeSignature({ subjectDigest: 'sha256:other' }) }), makeEvaluation())
    expect(result.status).toBe('blocked')
  })

  it('blocks when evidence is missing', () => {
    expect(evaluateImageGate(undefined, undefined).status).toBe('blocked')
  })
})
