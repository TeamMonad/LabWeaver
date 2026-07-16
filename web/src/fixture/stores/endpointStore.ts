import type { EnvironmentEndpointSchema } from '@/generated/contracts'
import type { EnvironmentInstanceSchema } from '@/generated/contracts'
import { nowIso } from '../utils/clock'
import { nextRevision } from '../utils/sequence'

const endpointsByEnvironment = new Map<string, EnvironmentEndpointSchema[]>()

export function seedEndpointsFor(environmentId: string, protocols: Array<{ id: string; protocol: EnvironmentEndpointSchema['protocol']; health?: EnvironmentEndpointSchema['health'] }>): void {
  const endpoints: EnvironmentEndpointSchema[] = protocols.map((p) => ({
    id: p.id,
    protocol: p.protocol,
    health: p.health ?? 'healthy',
    revision: nextRevision(),
    observedAt: nowIso(),
  }))
  endpointsByEnvironment.set(environmentId, endpoints)
}

export function listEndpoints(environmentId: string): EnvironmentEndpointSchema[] {
  return endpointsByEnvironment.get(environmentId) ?? []
}

export function getEndpoint(environmentId: string, endpointId: string): EnvironmentEndpointSchema | undefined {
  return listEndpoints(environmentId).find((e) => e.id === endpointId)
}

export function updateEndpointHealth(environmentId: string, endpointId: string, health: EnvironmentEndpointSchema['health']): void {
  const endpoints = endpointsByEnvironment.get(environmentId)
  if (!endpoints) return
  const endpoint = endpoints.find((e) => e.id === endpointId)
  if (!endpoint) return
  endpoint.health = health
  endpoint.revision = nextRevision()
  endpoint.observedAt = nowIso()
}

export function toEnvironmentEndpoints(environmentId: string): EnvironmentInstanceSchema['endpoints'] {
  return listEndpoints(environmentId).map((e) => ({
    id: e.id,
    protocol: e.protocol,
    health: e.health,
    revision: e.revision,
    observedAt: e.observedAt,
  }))
}

export function resetEndpointStore(): void {
  endpointsByEnvironment.clear()
}
