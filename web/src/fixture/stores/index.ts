import { resetSshKeyStore } from '../handlers/sshKeys'
import { resetClock } from '../utils/clock'
import { resetSequence } from '../utils/sequence'
import { resetAccessGrantStore, seedAccessGrants } from './accessGrantStore'
import { resetAgentRunStore } from './agentRunStore'
import { resetApprovalStore } from './approvalStore'
import { resetCandidateStore } from './candidateStore'
import { resetEndpointStore } from './endpointStore'
import { resetEnvironmentStore, seedEnvironments } from './environmentStore'
import { resetEvaluationStore, seedEvaluationData } from './evaluationStore'
import { resetEventLog } from './eventLog'
import { resetLlmPolicyStore, seedLlmPolicies } from './llmPolicyStore'
import { resetOperationStore, seedTimedOutOperation } from './operationStore'
import { resetProblemPackageStore } from './problemPackageStore'
import { resetResourceStore, seedResources } from './resourceStore'
import { resetTemplateReleaseStore, seedTemplateReleases } from './templateReleaseStore'
import { resetConsoleCapabilityStore } from './consoleCapabilityStore'

/**
 * Resets every fixture store, sequence, and clock offset to a deterministic
 * baseline. Useful for test isolation and for re-seeding fixture state.
 */
export function resetFixtureState(): void {
  resetSequence()
  resetClock()
  resetAccessGrantStore()
  resetAgentRunStore()
  resetApprovalStore()
  resetCandidateStore()
  resetEndpointStore()
  resetOperationStore()
  resetEnvironmentStore()
  resetEvaluationStore()
  resetEventLog()
  resetLlmPolicyStore()
  resetProblemPackageStore()
  resetResourceStore()
  resetSshKeyStore()
  resetTemplateReleaseStore()
  resetConsoleCapabilityStore()
  const envIds = seedEnvironments()
  seedAccessGrants(envIds)
  seedTimedOutOperation(envIds[0])
  seedTemplateReleases(['course-101', 'course-102', 'course-admin'])
  seedLlmPolicies(['course-101', 'course-102', 'course-admin'])
  seedEvaluationData()
  seedResources()
}

export * from './actorStore'
export * from './accessGrantStore'
export * from './agentRunStore'
export * from './endpointStore'
export * from './environmentStore'
export * from './evaluationStore'
export * from './eventLog'
export * from './llmPolicyStore'
export * from './operationStore'
export * from './problemPackageStore'
export * from './resourceStore'
