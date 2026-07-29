import type {
  ConsoleCapabilityAvailabilitySchema,
  ConsoleCapabilitySchema,
  ConsoleKind,
} from '@/generated/contracts'
import { nowIso } from '../utils/clock'
import { nextUuid7 } from '../utils/identity'
import { nextRevision } from '../utils/sequence'

/**
 * One-time console capability issuance. The connectionLocator is a same-origin
 * relative locator consumed once; the handoff secret is modelled as an
 * HttpOnly cookie concept and never stored or returned here.
 */
const issuedByGrant = new Map<string, ConsoleCapabilitySchema[]>()

export function availabilityFor(
  grantId: string,
  grantRevision: number,
  environment: { id: string; class: 'experiment' | 'work'; revision: number; eligibilityExpiresAt: string },
  kinds: ConsoleKind[],
  leaseFence?: ConsoleCapabilityAvailabilitySchema['leaseFence'],
): ConsoleCapabilityAvailabilitySchema {
  return {
    accessGrantId: grantId,
    accessGrantRevision: grantRevision,
    environmentClass: environment.class,
    environmentId: environment.id,
    environmentRevision: environment.revision,
    expiresAt: environment.eligibilityExpiresAt,
    kinds,
    leaseFence: leaseFence ?? null,
  }
}

export function issueCapability(
  grantId: string,
  grantRevision: number,
  environment: { id: string; class: 'experiment' | 'work'; revision: number; eligibilityExpiresAt: string },
  kind: ConsoleKind,
  leaseFence?: ConsoleCapabilityAvailabilitySchema['leaseFence'],
): ConsoleCapabilitySchema {
  const capability: ConsoleCapabilitySchema = {
    id: nextUuid7('consolecap'),
    accessGrantId: grantId,
    accessGrantRevision: grantRevision,
    environmentClass: environment.class,
    environmentId: environment.id,
    environmentRevision: environment.revision,
    kind,
    connectionLocator: `/api/v1/console-sessions/${nextUuid7('session')}`,
    websocketSubprotocol: `labweaver.console.${kind}.v1`,
    issuedAt: nowIso(),
    expiresAt: environment.eligibilityExpiresAt,
    leaseFence: leaseFence ?? null,
  }
  const existing = issuedByGrant.get(grantId) ?? []
  issuedByGrant.set(grantId, [...existing, capability])
  return capability
}

export function resetConsoleCapabilityStore(): void {
  issuedByGrant.clear()
}

export function bumpSequence(): number {
  return nextRevision()
}
