import axios, { type AxiosError, type AxiosRequestConfig, type AxiosResponse, InternalAxiosRequestConfig } from 'axios'
import { dispatch } from './handlers'
import { demoDelayMs } from './scenarioFlags'
import type { FixtureRequest } from './types'

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

function parseBody(raw: unknown): unknown {
  if (typeof raw === 'string' && raw.length > 0) {
    try {
      return JSON.parse(raw)
    } catch {
      return raw
    }
  }
  return raw
}

function extractRequestParts(config: AxiosRequestConfig): Pick<FixtureRequest, 'url' | 'headers' | 'body'> {
  const rawUrl = config.url as string | { url?: string; headers?: unknown; body?: unknown } | undefined

  if (typeof rawUrl === 'string') {
    return {
      url: rawUrl,
      headers: normalizeHeaders(config.headers),
      body: parseBody(config.data),
    }
  }

  if (rawUrl && typeof rawUrl === 'object') {
    return {
      url: rawUrl.url ?? config.baseURL ?? '/',
      headers: normalizeHeaders({ ...((rawUrl.headers ?? {}) as object), ...(config.headers ?? {}) }),
      body: parseBody(rawUrl.body ?? config.data),
    }
  }

  return {
    url: config.baseURL ?? '/',
    headers: normalizeHeaders(config.headers),
    body: parseBody(config.data),
  }
}

export async function fixtureAdapter(config: AxiosRequestConfig): Promise<AxiosResponse> {
  // Optional deterministic delay so loading states are demonstrable.
  const delay = demoDelayMs()
  if (delay > 0) {
    await new Promise((resolve) => setTimeout(resolve, delay))
  }

  const method = (config.method ?? 'GET').toUpperCase()
  const { url, headers, body } = extractRequestParts(config)

  const req: FixtureRequest = {
    ...config,
    method,
    url,
    headers,
    body,
  }

  const result = await dispatch(req)

  if (result instanceof Response) {
    const text = await result.text()
    const response: AxiosResponse = {
      data: text,
      status: result.status,
      statusText: `${result.status}`,
      headers: Object.fromEntries(result.headers.entries()),
      config: config as InternalAxiosRequestConfig,
      request: undefined,
    }
    if (result.status >= 400) {
      return Promise.reject(
        new axios.AxiosError(
          `fixture request failed: ${method} ${url}`,
          undefined,
          config as InternalAxiosRequestConfig,
          undefined,
          response,
        ) as AxiosError,
      )
    }
    return response
  }

  const response: AxiosResponse = {
    data: result.data,
    status: result.status,
    statusText: `${result.status}`,
    headers: result.headers ?? { 'Content-Type': 'application/json' },
    config: config as InternalAxiosRequestConfig,
    request: undefined,
  }

  if (result.status >= 400) {
    const error = new axios.AxiosError(
      `fixture request failed: ${method} ${url}`,
      undefined,
      config as InternalAxiosRequestConfig,
      undefined,
      response,
    ) as AxiosError
    return Promise.reject(error)
  }

  return response
}
