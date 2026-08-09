import { notFound, unauthorized, missingHeader, forbidden } from '../diagnostics'
import { can, parseActor, type FixtureActor } from '../stores/actorStore'
import type { FixtureHandler, FixtureHandlerResult, FixtureRequest, FixtureResponse } from '../types'
import * as accessGrants from './accessGrants'
import * as agentRuns from './agentRuns'
import * as candidates from './candidates'
import * as consoleCapabilities from './consoleCapabilities'
import * as environmentAccessGrants from './environmentAccessGrants'
import * as environmentEndpoints from './environmentEndpoints'
import * as environmentOperations from './environmentOperations'
import * as environments from './environments'
import * as evaluation from './evaluation'
import * as events from './events'
import * as frozenSubmissions from './frozenSubmissions'
import * as llmPolicy from './llmPolicy'
import * as problemPackages from './problemPackages'
import * as resource from './resource'
import * as sshKeys from './sshKeys'
import * as templateReleases from './templateReleases'

interface RouteEntry {
  method: string
  match: (url: string) => boolean
  handler: FixtureHandler
}

const routes: RouteEntry[] = [
  // Evaluation release lifecycle and owner-scoped terminal results
  { method: 'GET', match: (url) => /^\/api\/v1\/courses\/[^/]+\/evaluation-releases$/.test(url), handler: evaluation.listReleases },
  { method: 'POST', match: (url) => /^\/api\/v1\/courses\/[^/]+\/evaluation-releases$/.test(url), handler: evaluation.createRelease },
  { method: 'GET', match: (url) => /^\/api\/v1\/courses\/[^/]+\/evaluation-releases\/[^/]+$/.test(url), handler: evaluation.getRelease },
  { method: 'POST', match: (url) => /^\/api\/v1\/courses\/[^/]+\/evaluation-releases\/[^/]+\/withdraw$/.test(url), handler: evaluation.withdrawRelease },
  { method: 'GET', match: (url) => /^\/api\/v1\/courses\/[^/]+\/me\/evaluation-results$/.test(url), handler: evaluation.listResults },
  { method: 'GET', match: (url) => /^\/api\/v1\/courses\/[^/]+\/me\/evaluation-results\/[^/]+$/.test(url), handler: evaluation.getResult },

  // Environment template releases
  { method: 'GET', match: (url) => /^\/api\/v1\/courses\/[^/]+\/environment-template-releases$/.test(url), handler: templateReleases.listEnvironmentTemplateReleases },
  { method: 'POST', match: (url) => /^\/api\/v1\/courses\/[^/]+\/environment-template-releases$/.test(url), handler: templateReleases.createEnvironmentTemplateRelease },

  // Candidates
  { method: 'GET', match: (url) => /^\/api\/v1\/courses\/[^/]+\/environment-candidates\/[^/]+$/.test(url), handler: candidates.getEnvironmentCandidateHandler },
  { method: 'POST', match: (url) => /^\/api\/v1\/courses\/[^/]+\/environment-candidates\/[^/]+\/decisions$/.test(url), handler: candidates.appendEnvironmentCandidateDecision },
  { method: 'GET', match: (url) => /^\/api\/v1\/courses\/[^/]+\/evaluation-candidates\/[^/]+$/.test(url), handler: candidates.getEvaluationCandidateHandler },
  { method: 'POST', match: (url) => /^\/api\/v1\/courses\/[^/]+\/evaluation-candidates\/[^/]+\/decisions$/.test(url), handler: candidates.appendEvaluationCandidateDecision },

  // Course LLM egress policy
  { method: 'GET', match: (url) => /^\/api\/v1\/courses\/[^/]+\/llm-egress-policies\/active$/.test(url), handler: llmPolicy.getActiveCourseLlmPolicy },

  // Problem package uploads
  { method: 'POST', match: (url) => /^\/api\/v1\/courses\/[^/]+\/problem-package-uploads$/.test(url), handler: problemPackages.createProblemPackageUpload },
  { method: 'POST', match: (url) => /^\/api\/v1\/courses\/[^/]+\/problem-package-uploads\/[^/]+\/complete$/.test(url), handler: problemPackages.completeProblemPackageUpload },

  // Agent runs
  { method: 'POST', match: (url) => /^\/api\/v1\/courses\/[^/]+\/agent-runs$/.test(url), handler: agentRuns.createAgentRun },
  { method: 'GET', match: (url) => /^\/api\/v1\/courses\/[^/]+\/agent-runs\/[^/]+$/.test(url), handler: agentRuns.getAgentRun },
  { method: 'POST', match: (url) => /^\/api\/v1\/courses\/[^/]+\/agent-runs\/[^/]+\/cancel$/.test(url), handler: agentRuns.cancelAgentRun },
  { method: 'POST', match: (url) => /^\/api\/v1\/courses\/[^/]+\/agent-runs\/[^/]+\/tracks\/[^/]+\/retry$/.test(url), handler: agentRuns.retryAgentRunTrack },

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
  { method: 'GET', match: (url) => /^\/api\/v1\/frozen-submissions\/[^/]+$/.test(url), handler: frozenSubmissions.getFrozenSubmissionHandler },

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

  // Resource requests and leases
  { method: 'GET', match: (url) => url === '/api/v1/resource-requests', handler: resource.listResourceRequests },
  { method: 'POST', match: (url) => url === '/api/v1/resource-requests', handler: resource.createResourceRequest },
  { method: 'GET', match: (url) => /^\/api\/v1\/resource-requests\/[^/]+$/.test(url), handler: resource.getResourceRequest },
  { method: 'POST', match: (url) => /^\/api\/v1\/resource-requests\/[^/]+\/approve$/.test(url), handler: resource.approveResourceRequest },
  { method: 'POST', match: (url) => /^\/api\/v1\/resource-requests\/[^/]+\/resize-and-approve$/.test(url), handler: resource.resizeAndApproveResourceRequest },
  { method: 'POST', match: (url) => /^\/api\/v1\/resource-requests\/[^/]+\/reject$/.test(url), handler: resource.rejectResourceRequest },
  { method: 'POST', match: (url) => /^\/api\/v1\/resource-requests\/[^/]+\/cancel$/.test(url), handler: resource.cancelResourceRequest },
  { method: 'POST', match: (url) => /^\/api\/v1\/resource-requests\/[^/]+\/retry$/.test(url), handler: resource.retryResourceRequest },
  { method: 'GET', match: (url) => url === '/api/v1/resource-leases', handler: resource.listResourceLeases },
  { method: 'GET', match: (url) => /^\/api\/v1\/resource-leases\/[^/]+$/.test(url), handler: resource.getResourceLease },
  { method: 'POST', match: (url) => /^\/api\/v1\/resource-leases\/[^/]+\/renew$/.test(url), handler: resource.renewResourceLease },
  { method: 'POST', match: (url) => /^\/api\/v1\/resource-leases\/[^/]+\/revoke$/.test(url), handler: resource.revokeResourceLease },

  // Console capabilities
  { method: 'GET', match: (url) => /^\/api\/v1\/access-grants\/[^/]+\/console-capabilities$/.test(url), handler: consoleCapabilities.listConsoleCapabilities },
  { method: 'POST', match: (url) => /^\/api\/v1\/access-grants\/[^/]+\/console-capabilities$/.test(url), handler: consoleCapabilities.issueConsoleCapability },

  // Events
  { method: 'GET', match: (url) => url === '/api/v1/events', handler: events.streamEvents },

  // SSH public keys
  { method: 'GET', match: (url) => url === '/api/v1/me/ssh-public-keys', handler: sshKeys.listSshPublicKeys },
  { method: 'POST', match: (url) => url === '/api/v1/me/ssh-public-keys', handler: sshKeys.createSshPublicKey },
  { method: 'DELETE', match: (url) => /^\/api\/v1\/me\/ssh-public-keys\/.+/.test(url), handler: sshKeys.deleteSshPublicKey },
]

export function dispatch(req: FixtureRequest): FixtureHandlerResult {
  const method = req.method.toUpperCase()
  // Match routes on the pathname only; handlers still receive the full URL
  // (with query string) so query-aware handlers like the SSE stream can parse
  // parameters themselves.
  const pathname = req.url.split('?')[0].split('#')[0]
  const route = routes.find((r) => r.method === method && r.match(pathname))
  if (!route) {
    return notFound(`${method} ${req.url}`)
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

/**
 * Parse the revision from an If-Match header.
 *
 * Mirrors `StrongEtag::parse` (crates/contracts/src/http.rs): the value must be
 * the quoted `"rev-<n>"` strong validator. Weak validators (`W/` prefix),
 * unquoted values and zero revisions are rejected so fixture behavior matches
 * the real backend contract.
 */
export function parseIfMatchRevision(value: string): number | null {
  const trimmed = value.trim()
  if (trimmed.startsWith('W/')) return null
  const match = /^"rev-(\d+)"$/.exec(trimmed)
  if (!match) return null
  const parsed = Number(match[1])
  if (!Number.isSafeInteger(parsed) || parsed <= 0) return null
  return parsed
}
