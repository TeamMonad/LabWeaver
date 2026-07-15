import type { AxiosRequestConfig } from 'axios'

export interface FixtureRequest extends AxiosRequestConfig {
  method: string
  url: string
  headers: Record<string, string>
  body?: unknown
}

export interface FixtureResponse<T = unknown> {
  status: number
  data: T
  headers?: Record<string, string>
}

export type FixtureHandler = (req: FixtureRequest) => FixtureResponse | Promise<FixtureResponse>

export interface FixtureRoute {
  method: string
  path: string
  handler: FixtureHandler
}
