import type { AccessGrantSchema, EndpointGrant, EnvironmentInstanceSchema } from '@/generated/contracts'

export type AccessGrantWithGateway = AccessGrantSchema

export type EnvironmentInstanceWithFreeze = EnvironmentInstanceSchema & {
  freezeEvidence?: EnvironmentInstanceSchema['cleanupEvidence']
}

/** Single-line SSH command for a VM endpoint grant. Returns null when the
 * identity is incomplete (fail closed). */
export function buildSshCommand(endpointGrant: EndpointGrant): string | null {
  if (!endpointGrant.alias) return null
  if (!endpointGrant.sshGatewayHostname || endpointGrant.sshGatewayPort !== 2222) return null
  return `ssh -p ${endpointGrant.sshGatewayPort} ${endpointGrant.alias}@${endpointGrant.sshGatewayHostname}`
}

/** Code-server connect URL for an https endpoint grant. Returns null when the
 * backend did not provide one (fail closed). */
export function resolveConnectUrl(endpointGrant: EndpointGrant): string | null {
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
