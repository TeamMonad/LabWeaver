import { resetSshKeyStore } from '../handlers/sshKeys'
import { resetClock } from '../utils/clock'
import { resetSequence } from '../utils/sequence'
import { resetAccessGrantStore, seedAccessGrants } from './accessGrantStore'
import { resetAgentRunStore } from './agentRunStore'
import { resetEndpointStore } from './endpointStore'
import { resetEnvironmentStore, seedEnvironments } from './environmentStore'
import { resetEventLog } from './eventLog'
import { resetLlmPolicyStore, seedLlmPolicies } from './llmPolicyStore'
import { resetOperationStore, seedTimedOutOperation } from './operationStore'
import { resetProblemPackageStore } from './problemPackageStore'
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
  resetEndpointStore()
  resetOperationStore()
  resetEnvironmentStore()
  resetEventLog()
  resetLlmPolicyStore()
  resetProblemPackageStore()
  resetSshKeyStore()
  resetTemplateReleaseStore()
  resetConsoleCapabilityStore()
  const envIds = seedEnvironments()
  seedAccessGrants(envIds)
  seedTimedOutOperation(envIds[0])
  seedTemplateReleases(['course-101', 'course-102', 'course-admin'])
  seedLlmPolicies(['course-101', 'course-102', 'course-admin'])
}

export * from './actorStore'
export * from './accessGrantStore'
export * from './agentRunStore'
export * from './endpointStore'
export * from './environmentStore'
export * from './eventLog'
export * from './llmPolicyStore'
export * from './operationStore'
export * from './problemPackageStore'
