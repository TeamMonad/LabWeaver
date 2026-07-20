import { spawn } from 'node:child_process'
import { stat } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { validateConfiguration, buildReport, writeReport } from './verify-config.mjs'

const WEB_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const REQUIRED_VALUES = Object.freeze([
  'LABWEAVER_TEACHER_USERNAME',
  'LABWEAVER_TEACHER_PASSWORD_FILE',
  'LABWEAVER_STUDENT_USERNAME',
  'LABWEAVER_STUDENT_PASSWORD_FILE',
  'LABWEAVER_E2E_AGENT_RUN_ID',
  'LABWEAVER_E2E_CONTAINER_ENVIRONMENT_ID',
  'LABWEAVER_E2E_VM_ENVIRONMENT_ID',
])
const PASSWORD_FILES = Object.freeze([
  'LABWEAVER_TEACHER_PASSWORD_FILE',
  'LABWEAVER_STUDENT_PASSWORD_FILE',
])

function fail(code, diagnostics) {
  if (!diagnostics.includes(code)) diagnostics.push(code)
}

function isHttpUrl(value) {
  try {
    const url = new URL(value)
    return url.protocol === 'http:' || url.protocol === 'https:'
  } catch {
    return false
  }
}

async function validateRuntimeInputs(environment, diagnostics) {
  for (const name of REQUIRED_VALUES) {
    if (!environment[name]?.trim()) fail(`PW_AUTH_CONFIGURATION_MISSING:${name}`, diagnostics)
  }
  for (const name of PASSWORD_FILES) {
    const fileName = environment[name]?.trim()
    if (!fileName) continue
    try {
      const metadata = await stat(fileName)
      if (!metadata.isFile() || metadata.size < 1 || metadata.size > 4096) {
        fail(`PW_AUTH_PASSWORD_FILE_INVALID:${name}`, diagnostics)
      }
    } catch {
      fail(`PW_AUTH_PASSWORD_FILE_INVALID:${name}`, diagnostics)
    }
  }
}

function executePlaywright(environment) {
  return new Promise((resolve) => {
    const child = spawn(
      process.execPath,
      ['node_modules/@playwright/test/cli.js', 'test', '--config=playwright.config.mjs'],
      { cwd: WEB_ROOT, env: environment, stdio: 'inherit' },
    )
    child.once('error', (error) => resolve({ exitCode: 1, error: error.message }))
    child.once('close', (code, signal) => resolve({
      exitCode: typeof code === 'number' ? code : 1,
      ...(signal ? { signal } : {}),
    }))
  })
}

export async function runE2e({ environment = process.env, execute = executePlaywright } = {}) {
  const { diagnostics } = await validateConfiguration({
    requirementsBaselineHead: environment.PW_REQUIREMENTS_BASELINE_HEAD,
  })
  const baseUrl = environment.LABWEAVER_BASE_URL
  if (!baseUrl) fail('PW_BASE_URL_REQUIRED', diagnostics)
  else if (!isHttpUrl(baseUrl)) fail('PW_BASE_URL_INVALID', diagnostics)
  if (diagnostics.length === 0) await validateRuntimeInputs(environment, diagnostics)

  if (diagnostics.length > 0) {
    const report = buildReport({ diagnostics, overall: 'blocked' })
    await writeReport(report, {
      reportPath: environment.LABWEAVER_E2E_REPORT_PATH
        ? path.resolve(environment.LABWEAVER_E2E_REPORT_PATH)
        : undefined,
    })
    return { exitCode: 2, report }
  }

  const execution = await execute({ ...environment, LABWEAVER_DATA_MODE: 'live' })
  const passed = execution.exitCode === 0
  const runtimeDiagnostics = passed ? [] : ['PW_RUNTIME_E2E_FAILED']
  const report = buildReport({
    diagnostics: runtimeDiagnostics,
    overall: passed ? 'passed' : 'failed',
    checks: {
      playwright: {
        status: passed ? 'passed' : 'failed',
        exitCode: execution.exitCode,
        ...(execution.error ? { error: execution.error } : {}),
        ...(execution.signal ? { signal: execution.signal } : {}),
      },
    },
    runtimeE2e: 'executed',
    evidenceLevel: 'E3',
  })
  await writeReport(report, {
    reportPath: environment.LABWEAVER_E2E_REPORT_PATH
      ? path.resolve(environment.LABWEAVER_E2E_REPORT_PATH)
      : undefined,
  })
  return { exitCode: passed ? 0 : 1, report }
}

async function main() {
  const result = await runE2e()
  for (const code of result.report.diagnostics) console.error(code)
  process.exitCode = result.exitCode
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main()
}
