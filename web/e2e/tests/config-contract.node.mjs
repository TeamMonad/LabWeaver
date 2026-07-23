import assert from 'node:assert/strict'
import test from 'node:test'
import { createPlaywrightConfig } from '../../playwright.config.mjs'
import { PROJECT_NAMES, REQUIREMENTS_BASELINE, ROLE_PROJECTS_BY_NAME } from '../config/role-projects.mjs'
import { buildReport, validateConfiguration } from '../../scripts/verify-config.mjs'

const dataMode = process.env.LABWEAVER_DATA_MODE || process.env.VITE_DATA_MODE || 'live'
const isFixture = dataMode === 'fixture'
const evidenceLabel = isFixture ? 'fixture' : 'live'

test('role projects are uniquely derived from the authoritative definition', () => {
  const config = createPlaywrightConfig({ ci: true })
  assert.deepEqual(config.projects.map((project) => project.name), PROJECT_NAMES)
  assert.equal(new Set(PROJECT_NAMES).size, 6)
  assert.equal(PROJECT_NAMES.includes('researcher'), false)
  for (const name of ['teacher', 'student', 'platform-admin']) {
    const project = config.projects.find((candidate) => candidate.name === name)
    assert.deepEqual(project.dependencies, ['setup'])
    assert.equal(project.use.storageState, ROLE_PROJECTS_BY_NAME[name].storageState)
    assert.match(project.use.storageState, /^\.auth\/[a-z-]+\.json$/)
    assert.equal(project.testIgnore.test(`${name}/example.live.spec.mjs`), isFixture)
    assert.equal(project.testIgnore.test(`${name}/example.fixture.spec.mjs`), !isFixture)
  }
  for (const name of ['visual-regression', 'a11y']) {
    const project = config.projects.find((candidate) => candidate.name === name)
    assert.equal(project.dependencies, undefined)
    assert.equal(project.use?.storageState, undefined)
  }
  assert.equal(ROLE_PROJECTS_BY_NAME.student.aliases.includes('researcher'), false)
  assert.equal(ROLE_PROJECTS_BY_NAME.student.testMatch.test('researcher/example.spec.mjs'), false)
  assert.equal(config.projects.some((project) => project.use?.storageState === '.auth/researcher.json'), false)
  assert.equal(ROLE_PROJECTS_BY_NAME['platform-admin'].aliases.includes('admin'), true)
  assert.equal(config.forbidOnly, true)
  assert.match(config.outputDir, /^\.\/test-results(?:\/(live|fixture))?$/)
  assert.equal(config.metadata.dataMode, dataMode)
  assert.equal(config.metadata.evidenceLabel, evidenceLabel)
  assert.match(config.metadata.sourceCommit, /^[0-9a-f]{40}$/i)
  if (isFixture) {
    assert.match(config.metadata.fixtureManifestHash, /^[0-9a-f]{16}$/i)
    assert.equal(typeof config.metadata.browser, 'string')
    assert.equal(typeof config.metadata.browserVersion, 'string')
    assert.deepEqual(config.metadata.viewport, { width: 1440, height: 900 })
  }
})

test('configuration contract retains failure artifacts and reports E1 only', async () => {
  const result = await validateConfiguration({ requirementsBaselineHead: REQUIREMENTS_BASELINE.head })
  assert.deepEqual(result.diagnostics, [])
  const config = createPlaywrightConfig({ ci: true })
  assert.equal(config.use.trace, 'retain-on-failure')
  assert.equal(config.use.screenshot, 'only-on-failure')
  assert.equal(config.use.video, 'retain-on-failure')
  const passed = buildReport({ diagnostics: [], overall: 'passed' })
  const failed = buildReport({ diagnostics: ['PW_FIXED_SLEEP_DETECTED'], overall: 'failed' })
  const blocked = buildReport({ diagnostics: ['PW_AUTH_SETUP_NOT_IMPLEMENTED'], overall: 'blocked' })
  assert.equal(passed.event, 'playwright_role_config_verified')
  assert.equal(failed.event, 'playwright_role_config_failed')
  assert.equal(blocked.event, 'playwright_role_config_blocked')
  assert.equal(passed.evidenceLevel, 'E1')
  assert.equal(passed.runtime_e2e, 'not_executed')
})

test('a changed provisional baseline is a fail-fast diagnostic', async () => {
  const result = await validateConfiguration({ requirementsBaselineHead: 'stale-head' })
  assert.deepEqual(result.diagnostics, ['PW_REQUIREMENTS_BASELINE_CHANGED'])
})

test('external web-server mode is explicit and never starts a local preview fallback', () => {
  const previousExternal = process.env.LABWEAVER_EXTERNAL_WEB_SERVER
  const previousBaseUrl = process.env.LABWEAVER_BASE_URL
  try {
    delete process.env.LABWEAVER_BASE_URL
    delete process.env.LABWEAVER_EXTERNAL_WEB_SERVER
    assert.equal(createPlaywrightConfig({ ci: true }).webServer.command.includes('preview'), true)

    process.env.LABWEAVER_EXTERNAL_WEB_SERVER = 'true'
    assert.equal(createPlaywrightConfig({ ci: true }).webServer, undefined)

    process.env.LABWEAVER_EXTERNAL_WEB_SERVER = 'false'
    assert.equal(createPlaywrightConfig({ ci: true }).webServer.command.includes('preview'), true)

    process.env.LABWEAVER_EXTERNAL_WEB_SERVER = '1'
    assert.throws(
      () => createPlaywrightConfig({ ci: true }),
      /LABWEAVER_EXTERNAL_WEB_SERVER=1/,
    )
  } finally {
    if (previousExternal === undefined) delete process.env.LABWEAVER_EXTERNAL_WEB_SERVER
    else process.env.LABWEAVER_EXTERNAL_WEB_SERVER = previousExternal
    if (previousBaseUrl === undefined) delete process.env.LABWEAVER_BASE_URL
    else process.env.LABWEAVER_BASE_URL = previousBaseUrl
  }
})
