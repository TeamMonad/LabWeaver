import { dispatch } from './handlers'
import type { FixtureRequest } from './types'

let originalFetch: typeof fetch | undefined

function normalizeHeaders(raw: unknown): Record<string, string> {
  const out: Record<string, string> = {}
  if (raw && typeof raw === 'object') {
    for (const [key, value] of Object.entries(raw)) {
      if (value !== undefined && value !== null) {
        out[key] = String(value)
      }
    }
  }
  return out
}

async function parseBody(raw: unknown): Promise<unknown> {
  if (raw instanceof Blob) {
    return raw.text().then((text) => parseJsonBody(text))
  }
  if (typeof raw === 'string') {
    return parseJsonBody(raw)
  }
  return raw
}

function parseJsonBody(raw: string): unknown {
  if (raw.length > 0) {
    try {
      return JSON.parse(raw)
    } catch {
      return raw
    }
  }
  return undefined
}

async function fixtureFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const request = input instanceof Request && init === undefined ? input : new Request(input, init)
  const url = new URL(request.url)

  if (!url.pathname.startsWith('/api/v1/')) {
    return (originalFetch ?? globalThis.fetch)(input, init)
  }

  const body = await parseBody(request.body as unknown)
  const req: FixtureRequest = {
    method: request.method.toUpperCase(),
    url: url.pathname + url.search,
    headers: normalizeHeaders(Object.fromEntries(request.headers.entries())),
    body,
  }

  const result = await dispatch(req)

  if (result instanceof Response) {
    return result
  }

  const responseBody = result.data === undefined ? null : JSON.stringify(result.data)
  return new Response(responseBody, {
    status: result.status,
    headers: result.headers ?? { 'Content-Type': 'application/json' },
  })
}

export function installFetchInterceptor(): void {
  if (originalFetch) return
  originalFetch = globalThis.fetch.bind(globalThis)
  globalThis.fetch = fixtureFetch
}

export function uninstallFetchInterceptor(): void {
  if (!originalFetch) return
  globalThis.fetch = originalFetch
  originalFetch = undefined
}
