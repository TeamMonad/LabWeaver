import type {
  CandidateApprovalSchema,
  EnvironmentCandidateSchema,
  EvaluationCandidateSchema,
  ImageArtifact,
  ImagePolicyEvaluation,
} from '@/generated/contracts'

/** Environment candidate response may include backend-provided scan evidence
 * as additive optional fields. The UI only consumes these values; it never
 * fabricates them. */
export type EnvironmentCandidateWithEvidence = EnvironmentCandidateSchema & {
  approvals?: CandidateApprovalSchema[]
  imageArtifact?: ImageArtifact
  imagePolicyEvaluation?: ImagePolicyEvaluation
  trustRevision?: number
}

export type EvaluationCandidateWithApprovals = EvaluationCandidateSchema & {
  approvals?: CandidateApprovalSchema[]
  trustRevision?: number
}

export type CandidateKind = 'environment' | 'evaluation'
export type CandidateDecision = 'approved' | 'rejected' | 'withdrawn'

export type ImageGateStatus = 'passed' | 'warning' | 'blocked'

export interface ImageGateResult {
  status: ImageGateStatus
  reasons: string[]
}

export function evaluateImageGate(
  artifact: ImageArtifact | undefined,
  evaluation: ImagePolicyEvaluation | undefined,
): ImageGateResult {
  if (!artifact || !evaluation) {
    return { status: 'blocked', reasons: ['发布证据尚未生成'] }
  }

  const reasons: string[] = []
  if (artifact.kind === 'container' && artifact.digest !== evaluation.artifactSha256) {
    reasons.push('镜像 digest 与扫描结果不一致')
  }

  if (evaluation.vulnerabilities.critical > 0) {
    reasons.push(`存在 ${evaluation.vulnerabilities.critical} 个 Critical 漏洞`)
  }

  if (!evaluation.passed) {
    reasons.push('Trivy Gate 未通过')
  }

  if (reasons.length > 0) {
    return { status: 'blocked', reasons }
  }

  if (evaluation.vulnerabilities.high > 0) {
    return { status: 'warning', reasons: [`存在 ${evaluation.vulnerabilities.high} 个 High 漏洞，可发布但需人工确认`] }
  }

  return { status: 'passed', reasons: [] }
}
