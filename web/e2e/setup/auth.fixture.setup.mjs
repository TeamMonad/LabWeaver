import { test } from '@playwright/test'
import { writeFixtureAuthStates } from './fixture-auth-state.mjs'

test('prepare fixture auth states', async () => {
  writeFixtureAuthStates()
})
