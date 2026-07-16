import { notFound, unauthorized, missingHeader, forbidden } from '../diagnostics'
import { can, parseActor, type FixtureActor } from '../stores/actorStore'
import type { FixtureHandler, FixtureHandlerResult, FixtureRequest, FixtureResponse } from '../types'
import * as accessGrants from './accessGrants'
import * as environmentAccessGrants from './environmentAccessGrants'
import * as environmentEndpoints from './environmentEndpoints'
import * as environmentOperations from './environmentOperations'
import * as environments from './environments'
import * as events from './events'
import * as sshKeys from './sshKeys'
import * as templateReleases from './templateReleases'

interface RouteEntry {
  method: string
  match: (url: string) => boolean
  handler: FixtureHandler
}

const routes: RouteEntry[] = [
  // Environment template releases
  { method: 'GET', match: (url) => /^\/api\/v1\/courses\/[^/]+\/environment-template-releases$/.test(url), handler: templateReleases.listEnvironmentTemplateReleases },

  // Environments
  { method: 'GET', match: (url) => url === '/api/v1/environments', handler: environments.listEnvironmentsHandler },
  { method: 'POST', match: (url) => url === '/api/v1/environments', handler: environments.createEnvironmentHandler },
  { method: 'GET', match: (url) => /^\/api\/v1\/environments\/[^/]+$/.test(url), handler: environments.getEnvironmentHandler },
  { method: 'DELETE', match: (url) => /^\/api\/v1\/environments\/[^/]+$/.test(url), handler: environments.deleteEnvironmentHandler },
  { method: 'POST', match: (url) => /^\/api\/v1\/environments\/[^/]+\/start$/.test(url), handler: environments.startEnvironment },
  { method: 'POST', match: (url) => /^\/api\/v1\/environments\/[^/]+\/stop$/.test(url), handler: environments.stopEnvironment },
  { method: 'POST', match: (url) => /^\/api\/v1\/environments\/[^/]+\/restart$/.test(url), handler: environments.restartEnvironment },
  { method: 'POST', match: (url) => /^\/api\/v1\/environments\/[^/]+\/reset$/.test(url), handler: environments.resetEnvironment },
  { method: 'POST', match: (url) => /^\/api\/v1\/environments\/[^/]+\/recover$/.test(url), handler: environments.recoverEnvironment },
  { method: 'POST', match: (url) => /^\/api\/v1\/environments\/[^/]+\/retry$/.test(url), handler: environments.retryEnvironment },
  { method: 'POST', match: (url) => /^\/api\/v1\/environments\/[^/]+\/cancel$/.test(url), handler: environments.cancelEnvironmentHandler },
  { method: 'POST', match: (url) => /^\/api\/v1\/environments\/[^/]+\/freeze$/.test(url), handler: environments.freezeEnvironmentHandler },

  // Environment endpoints
  { method: 'GET', match: (url) => /^\/api\/v1\/environments\/[^/]+\/endpoints$/.test(url), handler: environmentEndpoints.listEnvironmentEndpoints },

  // Environment access grants
  { method: 'GET', match: (url) => /^\/api\/v1\/environments\/[^/]+\/access-grants$/.test(url), handler: environmentAccessGrants.listEnvironmentAccessGrants },
  { method: 'POST', match: (url) => /^\/api\/v1\/environments\/[^/]+\/access-grants$/.test(url), handler: environmentAccessGrants.createEnvironmentAccessGrant },

  // Environment operations
  { method: 'GET', match: (url) => /^\/api\/v1\/environments\/[^/]+\/operations$/.test(url), handler: environmentOperations.listEnvironmentOperations },
  { method: 'GET', match: (url) => /^\/api\/v1\/environments\/[^/]+\/operations\/[^/]+$/.test(url), handler: environmentOperations.getEnvironmentOperation },

  // Access grants
  { method: 'GET', match: (url) => /^\/api\/v1\/access-grants\/[^/]+$/.test(url), handler: accessGrants.getAccessGrant },
  { method: 'POST', match: (url) => /^\/api\/v1\/access-grants\/[^/]+\/revoke$/.test(url), handler: accessGrants.revokeAccessGrant },

  // Events
  { method: 'GET', match: (url) => url === '/api/v1/events', handler: events.streamEvents },

  // SSH public keys
  { method: 'GET', match: (url) => url === '/api/v1/me/ssh-public-keys', handler: sshKeys.listSshPublicKeys },
  { method: 'POST', match: (url) => url === '/api/v1/me/ssh-public-keys', handler: sshKeys.createSshPublicKey },
  { method: 'DELETE', match: (url) => /^\/api\/v1\/me\/ssh-public-keys\/.+/.test(url), handler: sshKeys.deleteSshPublicKey },
]

export function dispatch(req: FixtureRequest): FixtureHandlerResult {
  const method = req.method.toUpperCase()
  const url = req.url
  const route = routes.find((r) => r.method === method && r.match(url))
  if (!route) {
    return notFound(`${method} ${url}`)
  }
  return route.handler(req)
}

export function requireActor(req: FixtureRequest): FixtureActor | FixtureResponse {
  const actor = parseActor(req)
  if (!actor) return unauthorized()
  return actor
}

export function requireIdempotencyKey(req: FixtureRequest): string | FixtureResponse {
  const key = req.headers['Idempotency-Key'] ?? req.headers['idempotency-key']
  if (!key) return missingHeader('Idempotency-Key')
  return key
}

export function requireIfMatch(req: FixtureRequest): string | FixtureResponse {
  const value = req.headers['If-Match'] ?? req.headers['if-match']
  if (!value) return missingHeader('If-Match')
  return value
}

export function requireRole(actor: FixtureActor, action: Parameters<typeof can>[1], resource?: Parameters<typeof can>[2]): true | FixtureResponse {
  if (!can(actor.role, action, resource)) {
    return forbidden()
  }
  return true
}

export function extractPathParam(url: string, pattern: RegExp, groupIndex: number): string | null {
  const match = pattern.exec(url)
  return match?.[groupIndex] ?? null
}
