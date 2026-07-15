import { execFileSync } from 'node:child_process'
import { mkdir, rename, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { createPlaywrightConfig } from '../playwright.config.mjs'
import { PROJECT_NAMES, REQUIREMENTS_BASELINE, ROLE_PROJECTS_BY_NAME } from '../e2e/config/role-projects.mjs'
import { findFixedSleeps } from './fixed-sleep-check.mjs'

const WEB_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const REPOSITORY_ROOT = path.resolve(WEB_ROOT, '..')
const DEFAULT_REPORT_PATH = path.join(WEB_ROOT, 'artifacts', 'playwright', 'playwright-role-config-report.json')

export function resolveReportPath() {
  return process.env.LABWEAVER_E2E_REPORT_PATH
    ? path.resolve(process.env.LABWEAVER_E2E_REPORT_PATH)
    : DEFAULT_REPORT_PATH
}

function currentCommit() {
  return execFileSync('git', ['rev-parse', 'HEAD'], { cwd: REPOSITORY_ROOT, encoding: 'utf8' }).trim()
}

function diagnostic(condition, code, diagnostics) {
  if (!condition) diagnostics.push(code)
}

export async function validateConfiguration({ requirementsBaselineHead } = {}) {
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
  diagnostic(config.forbidOnly === true, 'PW_PROJECT_SET_INVALID', diagnostics)
  diagnostic(config.outputDir === './test-results', 'PW_PROJECT_SET_INVALID', diagnostics)
  diagnostic(!requirementsBaselineHead || requirementsBaselineHead === REQUIREMENTS_BASELINE.head, 'PW_REQUIREMENTS_BASELINE_CHANGED', diagnostics)
  const fixedSleeps = await findFixedSleeps()
  if (fixedSleeps.length > 0) diagnostics.push('PW_FIXED_SLEEP_DETECTED')
  return { diagnostics: [...new Set(diagnostics)], fixedSleeps }
}

export function buildReport({ diagnostics, overall, checks, generatedAt = new Date().toISOString() }) {
  const event = overall === 'passed'
    ? 'playwright_role_config_verified'
    : overall === 'blocked'
      ? 'playwright_role_config_blocked'
      : 'playwright_role_config_failed'
  return {
    schemaVersion: 1,
    event,
    overall,
    repository: 'TeamMonad/LabWeaver',
    issue: 9,
    commit: currentCommit(),
    requirements_baseline: REQUIREMENTS_BASELINE,
    projects: PROJECT_NAMES,
    role_mapping: {
      teacher: ROLE_PROJECTS_BY_NAME.teacher.aliases,
      student: ROLE_PROJECTS_BY_NAME.student.aliases,
      'platform-admin': ROLE_PROJECTS_BY_NAME['platform-admin'].aliases,
    },
    trace: 'retain-on-failure',
    fixed_sleep_scan: diagnostics.includes('PW_FIXED_SLEEP_DETECTED') ? 'failed' : 'passed',
    runtime_e2e: 'not_executed',
    evidenceLevel: 'E1',
    generatedAt,
    diagnostics,
    ...(checks ? { checks } : {}),
  }
}

export async function writeReport(report, { reportPath = resolveReportPath() } = {}) {
  await mkdir(path.dirname(reportPath), { recursive: true })
  const temporaryPath = `${reportPath}.${process.pid}.tmp`
  await writeFile(temporaryPath, `${JSON.stringify(report, null, 2)}\n`, 'utf8')
  await rename(temporaryPath, reportPath)
}

async function main() {
  const { diagnostics, fixedSleeps } = await validateConfiguration({
    requirementsBaselineHead: process.env.PW_REQUIREMENTS_BASELINE_HEAD,
  })
  if (diagnostics.length > 0) {
    for (const code of diagnostics) console.error(code)
    for (const finding of fixedSleeps) console.error(finding)
    process.exitCode = 1
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main()
}
