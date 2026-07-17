import { resetSshKeyStore } from '../handlers/sshKeys'
import { resetClock } from '../utils/clock'
import { resetSequence } from '../utils/sequence'
import { resetAccessGrantStore, seedAccessGrants } from './accessGrantStore'
import { resetEndpointStore } from './endpointStore'
import { resetEnvironmentStore, seedEnvironments } from './environmentStore'
import { resetEventLog } from './eventLog'
import { resetOperationStore, seedTimedOutOperation } from './operationStore'
import { resetTemplateReleaseStore, seedTemplateReleases } from './templateReleaseStore'

/**
 * Resets every fixture store, sequence, and clock offset to a deterministic
 * baseline. Useful for test isolation and for re-seeding fixture state.
 */
export function resetFixtureState(): void {
  resetSequence()
  resetClock()
  resetAccessGrantStore()
  resetEndpointStore()
  resetOperationStore()
  resetEnvironmentStore()
  resetEventLog()
  resetSshKeyStore()
  resetTemplateReleaseStore()
  const envIds = seedEnvironments()
  seedAccessGrants(envIds)
  seedTimedOutOperation(envIds[0])
  seedTemplateReleases(['course-101', 'course-102', 'course-admin'])
}

export * from './actorStore'
export * from './accessGrantStore'
export * from './endpointStore'
export * from './environmentStore'
export * from './eventLog'
export * from './operationStore'
