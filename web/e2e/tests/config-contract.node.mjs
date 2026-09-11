import assert from 'node:assert/strict'
import test from 'node:test'
import { createPlaywrightConfig } from '../../playwright.config.mjs'
import { PROJECT_NAMES, ROLE_PROJECTS_BY_NAME } from '../config/role-projects.mjs'
import { validateConfiguration } from '../../scripts/verify-config.mjs'

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
    assert.equal(project.testIgnore, undefined)
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
  assert.equal(config.outputDir, './test-results')
  assert.equal('ignoreHTTPSErrors' in config.use, false)
  assert.equal('metadata' in config, false)
})

test('configuration contract retains Playwright debugging on failure', async () => {
  const result = await validateConfiguration()
  assert.deepEqual(result.diagnostics, [])
  const config = createPlaywrightConfig({ ci: true })
  assert.equal(config.use.trace, 'retain-on-failure')
  assert.equal(config.use.screenshot, 'only-on-failure')
  assert.equal(config.use.video, 'retain-on-failure')
})
