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

export type FixtureHandlerResult = FixtureResponse | Response | Promise<FixtureResponse | Response>

export type FixtureHandler = (req: FixtureRequest) => FixtureHandlerResult

export interface FixtureRoute {
  method: string
  path: string
  handler: FixtureHandler
}
