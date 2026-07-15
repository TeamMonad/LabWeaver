import type { AxiosInstance } from 'axios'
import { fixtureAdapter } from './adapter'

export function installFixtureAdapter(client: AxiosInstance): void {
  client.defaults.adapter = fixtureAdapter
}
