import type { CourseLlmEgressPolicySchema } from '@/generated/contracts'
import { nowIso } from '../utils/clock'

const FIXTURE_SHA256_RUNTIME_CONFIG = 'a'.repeat(64)
const FIXTURE_SHA256_WORKER_IMAGE = 'b'.repeat(64)

const policies = new Map<string, CourseLlmEgressPolicySchema>()

function buildPolicy(courseId: string, revision: number): CourseLlmEgressPolicySchema {
  return {
    id: `policy-${courseId}`,
    courseId,
    revision,
    activatedAt: nowIso(),
    binding: {
      claudeCodeVersion: '2.1.0',
      maxInFlightPerWorker: 2,
      model: 'claude-sonnet-4-5-20250929',
      runtimeBinding: 'fixture-claude-code-runtime',
      runtimeConfigSha256: FIXTURE_SHA256_RUNTIME_CONFIG,
      workerImageSha256: FIXTURE_SHA256_WORKER_IMAGE,
    },
    budget: {
      maxCostMicrousd: 500_000,
      maxInputTokens: 200_000,
      maxOutputTokens: 32_000,
      maxRequests: 8,
      maxSchemaRepairs: 2,
      maxTransientRetries: 3,
      timeoutMilliseconds: 600_000,
    },
    deniedDataClasses: [
      'secret',
      'token',
      'private_key',
      'personally_identifiable_information',
      'unallowlisted_student_submission',
    ],
    studentContentMode: 'manifest_allowlist_only',
  }
}

/** Seeds one deterministic active LLM egress policy per fixture course. */
export function seedLlmPolicies(courseIds: string[]): void {
  courseIds.forEach((courseId, index) => {
    policies.set(courseId, buildPolicy(courseId, index + 1))
  })
}

export function getActivePolicy(courseId: string): CourseLlmEgressPolicySchema | undefined {
  return policies.get(courseId)
}

/** Rotates the active policy to a new revision, simulating a concurrent edit. */
export function rotatePolicy(courseId: string): CourseLlmEgressPolicySchema | undefined {
  const current = policies.get(courseId)
  if (!current) return undefined
  const next: CourseLlmEgressPolicySchema = {
    ...current,
    id: `policy-${courseId}`,
    revision: current.revision + 1,
    activatedAt: nowIso(),
  }
  policies.set(courseId, next)
  return next
}

export function resetLlmPolicyStore(): void {
  policies.clear()
}
