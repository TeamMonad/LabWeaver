import type { AxiosInstance } from 'axios'
import { fixtureAdapter } from './adapter'
import { installFetchInterceptor } from './fetchInterceptor'
import { resetFixtureState } from './stores'

export function installFixtureAdapter(client: AxiosInstance): void {
  client.defaults.adapter = fixtureAdapter
  installFetchInterceptor()
  resetFixtureState()
}
