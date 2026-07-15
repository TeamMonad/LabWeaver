import { notFound } from '../diagnostics'
import { createSshPublicKey, deleteSshPublicKey, listSshPublicKeys } from './sshKeys'
import type { FixtureHandler, FixtureRequest, FixtureResponse } from '../types'

interface RouteEntry {
  method: string
  match: (url: string) => boolean
  handler: FixtureHandler
}

const routes: RouteEntry[] = [
  {
    method: 'GET',
    match: (url) => url === '/api/v1/me/ssh-public-keys',
    handler: listSshPublicKeys,
  },
  {
    method: 'POST',
    match: (url) => url === '/api/v1/me/ssh-public-keys',
    handler: createSshPublicKey,
  },
  {
    method: 'DELETE',
    match: (url) => /^\/api\/v1\/me\/ssh-public-keys\/.+/.test(url),
    handler: deleteSshPublicKey,
  },
]

export function dispatch(req: FixtureRequest): FixtureResponse | Promise<FixtureResponse> {
  const method = req.method.toUpperCase()
  const url = req.url
  const route = routes.find((r) => r.method === method && r.match(url))
  if (!route) {
    return notFound(`${method} ${url}`)
  }
  return route.handler(req)
}
