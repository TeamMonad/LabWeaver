import path from 'node:path'
import {writeFixtureAuthStates} from '../e2e/setup/fixture-auth-state.mjs'

const files = writeFixtureAuthStates(path.resolve('.auth'))
process.stdout.write(`${JSON.stringify({event: 'fixture_auth_written', files: files.sort()})}\n`)
