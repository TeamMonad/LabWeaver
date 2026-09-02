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
const consumedLocators = new Set<string>()

/**
 * Consume a connection locator exactly once, mirroring ADR 0012's one-time
 * handoff: the first consumer succeeds, every subsequent consumer is denied.
 */
export function consumeLocator(locator: string): boolean {
  if (consumedLocators.has(locator)) return false
  consumedLocators.add(locator)
  return true
}

export function isLocatorConsumed(locator: string): boolean {
  return consumedLocators.has(locator)
}

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
  const issuedAt = nowIso()
  const expiresAt = new Date(Date.parse(issuedAt) + 30_000).toISOString()
  const capability: ConsoleCapabilitySchema = {
    id: nextUuid7('consolecap'),
    accessGrantId: grantId,
    accessGrantRevision: grantRevision,
    environmentClass: environment.class,
    environmentId: environment.id,
    environmentRevision: environment.revision,
    kind,
    connectionLocator: `/connect/console/${nextUuid7('session')}`,
    websocketSubprotocol: `labweaver.console.${kind}.v1`,
    issuedAt,
    expiresAt,
    leaseFence: leaseFence ?? null,
  }
  const existing = issuedByGrant.get(grantId) ?? []
  issuedByGrant.set(grantId, [...existing, capability])
  return capability
}

export function resetConsoleCapabilityStore(): void {
  issuedByGrant.clear()
  consumedLocators.clear()
}

export function bumpSequence(): number {
  return nextRevision()
}
