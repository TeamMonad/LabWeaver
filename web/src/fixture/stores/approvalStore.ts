import type { CandidateApprovalSchema, CandidateDecisionRequestSchema } from '@/generated/contracts'
import { nowIso } from '../utils/clock'
import { nextUuid7 } from '../utils/identity'

const approvalsByCandidate = new Map<string, CandidateApprovalSchema[]>()

export function appendDecision(
  candidateId: string,
  request: CandidateDecisionRequestSchema,
  actorId: string,
): CandidateApprovalSchema {
  const approval: CandidateApprovalSchema = {
    actorId,
    candidateId,
    candidateRevision: request.candidateRevision,
    candidateSha256: request.candidateSha256,
    decidedAt: nowIso(),
    decision: request.decision,
    id: nextUuid7('approval'),
    policyRevision: request.policyRevision,
    reason: request.reason,
    schemaSha256: request.schemaSha256,
    trustRevision: request.trustRevision,
  }
  const existing = approvalsByCandidate.get(candidateId) ?? []
  approvalsByCandidate.set(candidateId, [...existing, approval])
  return approval
}

export function getApprovals(candidateId: string): CandidateApprovalSchema[] {
  return approvalsByCandidate.get(candidateId) ?? []
}

export function getLatestApproval(candidateId: string): CandidateApprovalSchema | undefined {
  const approvals = approvalsByCandidate.get(candidateId) ?? []
  return approvals[approvals.length - 1]
}

export function resetApprovalStore(): void {
  approvalsByCandidate.clear()
}
