import { buildSshCommand, resolveConnectUrl, type AccessGrantWithGateway } from '@/types/access'

function endpointGrantOf(g: AccessGrantWithGateway, protocol: 'http' | 'https' | 'ssh') {
  return g.endpointGrants.find((eg) => eg.protocol === protocol && eg.health === 'healthy') ?? null
}

export function webGrant(g: AccessGrantWithGateway) {
  return endpointGrantOf(g, 'https') ?? endpointGrantOf(g, 'http')
}

export function sshGrant(g: AccessGrantWithGateway) {
  return endpointGrantOf(g, 'ssh')
}

export function connectUrl(g: AccessGrantWithGateway): string | null {
  const eg = webGrant(g)
  return eg ? resolveConnectUrl(eg) : null
}

export function openContainerRuntime(g: AccessGrantWithGateway) {
  const url = connectUrl(g)
  if (url) window.open(url, '_blank', 'noopener,noreferrer')
}

export function sshCommand(g: AccessGrantWithGateway): string | null {
  const eg = sshGrant(g)
  return eg ? buildSshCommand(eg) : null
}

export function sshFingerprint(g: AccessGrantWithGateway): string | null {
  return sshGrant(g)?.sshGatewayHostKeyFingerprint ?? null
}
