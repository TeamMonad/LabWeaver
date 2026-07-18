import type { AccessGrantSchema, EndpointGrant, EnvironmentInstanceSchema } from '@/generated/contracts'

/** Additive optional gateway fields the backend may attach to an AccessGrant
 * response. The UI only consumes these values; it never fabricates them. */
export type AccessGrantWithGateway = AccessGrantSchema & {
  gatewayHostname?: string
  gatewayFingerprintSha256?: string
  endpointGrants: Array<
    EndpointGrant & {
      connectUrl?: string | null
    }
  >
}

export type EnvironmentInstanceWithFreeze = EnvironmentInstanceSchema & {
  freezeEvidence?: EnvironmentInstanceSchema['cleanupEvidence']
}

/** Single-line SSH command for a VM endpoint grant. Returns null when the
 * identity is incomplete (fail closed). */
export function buildSshCommand(grant: AccessGrantWithGateway, endpointGrant: EndpointGrant): string | null {
  if (!endpointGrant.alias) return null
  if (!grant.gatewayHostname) return null
  return `ssh ${endpointGrant.alias}@${grant.gatewayHostname}`
}

/** Code-server connect URL for an https endpoint grant. Returns null when the
 * backend did not provide one (fail closed). */
export function resolveConnectUrl(endpointGrant: EndpointGrant & { connectUrl?: string | null }): string | null {
  return endpointGrant.connectUrl ?? null
}

/** Human-readable relative expiry such as "58 分钟后过期" / "已过期". */
export function formatExpiry(expiresAt: string, now: Date = new Date()): string {
  const expiry = new Date(expiresAt)
  const diffMs = expiry.getTime() - now.getTime()
  if (diffMs <= 0) return '已过期'
  const minutes = Math.floor(diffMs / 60000)
  if (minutes < 60) return `${minutes} 分钟后过期`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时后过期`
  const days = Math.floor(hours / 24)
  return `${days} 天后过期`
}
