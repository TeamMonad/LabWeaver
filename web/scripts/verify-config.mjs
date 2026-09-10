import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { createPlaywrightConfig } from '../playwright.config.mjs'
import { PROJECT_NAMES, ROLE_PROJECTS_BY_NAME } from '../e2e/config/role-projects.mjs'
import { findFixedSleeps } from './fixed-sleep-check.mjs'

function diagnostic(condition, code, diagnostics) {
  if (!condition) diagnostics.push(code)
}

export async function validateConfiguration() {
  const diagnostics = []
  const config = createPlaywrightConfig({ ci: true })
  const names = config.projects.map((project) => project.name)
  diagnostic(JSON.stringify(names) === JSON.stringify(PROJECT_NAMES), 'PW_PROJECT_SET_INVALID', diagnostics)
  diagnostic(new Set(names).size === names.length, 'PW_PROJECT_SET_INVALID', diagnostics)
  diagnostic(config.projects.some((project) => project.name === 'setup'), 'PW_SETUP_PROJECT_MISSING', diagnostics)
  for (const name of PROJECT_NAMES.filter((name) => name !== 'setup')) {
    const project = config.projects.find((candidate) => candidate.name === name)
    diagnostic(Boolean(project), 'PW_ROLE_PROJECT_MISSING', diagnostics)

    const expectedStorageState = ROLE_PROJECTS_BY_NAME[name].storageState
    if (expectedStorageState) {
      diagnostic(project?.dependencies?.length === 1 && project.dependencies[0] === 'setup', 'PW_ROLE_PROJECT_MISSING', diagnostics)
      diagnostic(project?.use?.storageState === expectedStorageState, 'PW_STORAGE_STATE_MISSING', diagnostics)
    }
  }
  const storageStates = config.projects
    .filter((project) => project.name !== 'setup' && ROLE_PROJECTS_BY_NAME[project.name]?.storageState)
    .map((project) => project.use?.storageState)
  diagnostic(storageStates.every((state) => typeof state === 'string' && state.startsWith('.auth/')), 'PW_STORAGE_STATE_MISSING', diagnostics)
  diagnostic(new Set(storageStates).size === storageStates.length, 'PW_STORAGE_STATE_MISSING', diagnostics)
  diagnostic(!names.includes('researcher'), 'PW_PROJECT_SET_INVALID', diagnostics)
  diagnostic(!ROLE_PROJECTS_BY_NAME.student.aliases.includes('researcher'), 'PW_RESEARCHER_ROLE_UNCONFIGURED', diagnostics)
  diagnostic(!ROLE_PROJECTS_BY_NAME.student.testMatch.test('researcher/example.spec.mjs'), 'PW_RESEARCHER_ROLE_UNCONFIGURED', diagnostics)
  diagnostic(ROLE_PROJECTS_BY_NAME['platform-admin'].aliases.includes('admin'), 'PW_ROLE_PROJECT_MISSING', diagnostics)
  diagnostic(config.use.trace === 'retain-on-failure', 'PW_TRACE_RETENTION_DISABLED', diagnostics)
  diagnostic(config.use.screenshot === 'only-on-failure', 'PW_TRACE_RETENTION_DISABLED', diagnostics)
  diagnostic(config.use.video === 'retain-on-failure', 'PW_TRACE_RETENTION_DISABLED', diagnostics)
  diagnostic(!('ignoreHTTPSErrors' in config.use), 'PW_TLS_VERIFICATION_DISABLED', diagnostics)
  diagnostic(config.forbidOnly === true, 'PW_PROJECT_SET_INVALID', diagnostics)
  diagnostic(config.outputDir === './test-results', 'PW_PROJECT_SET_INVALID', diagnostics)
  const fixedSleeps = await findFixedSleeps()
  if (fixedSleeps.length > 0) diagnostics.push('PW_FIXED_SLEEP_DETECTED')
  return { diagnostics: [...new Set(diagnostics)], fixedSleeps }
}

async function main() {
  const { diagnostics, fixedSleeps } = await validateConfiguration()
  if (diagnostics.length > 0) {
    for (const code of diagnostics) console.error(code)
    for (const finding of fixedSleeps) console.error(finding)
    process.exitCode = 1
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main()
}
