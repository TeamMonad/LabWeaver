import { spawn } from 'node:child_process'
import { readdir, rm } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { buildReport, resolveReportPath, writeReport } from './verify-config.mjs'

const WEB_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const CHECKS = Object.freeze(['verify', 'contract', 'list'])

function execute(command, args) {
  return new Promise((resolve) => {
    const child = spawn(command, args, { cwd: WEB_ROOT, stdio: 'inherit' })
    child.once('error', (error) => resolve({ exitCode: 1, error: error.message }))
    child.once('close', (code, signal) => resolve({
      exitCode: typeof code === 'number' ? code : 1,
      ...(signal ? { signal } : {}),
    }))
  })
}

async function runDefaultCheck(name) {
  if (name === 'verify') return execute(process.execPath, ['scripts/verify-config.mjs'])
  if (name === 'contract') {
    const tests = (await readdir(path.join(WEB_ROOT, 'e2e', 'tests')))
      .filter((file) => file.endsWith('.node.mjs'))
      .sort()
      .map((file) => path.join('e2e', 'tests', file))
    return execute(process.execPath, ['--test', ...tests])
  }
  return execute(process.execPath, [
    'node_modules/@playwright/test/cli.js',
    'test',
    '--list',
    '--config=playwright.config.mjs',
  ])
}

export async function runGate({ reportPath = resolveReportPath(), runCheck = runDefaultCheck } = {}) {
  // Do not allow an artifact upload to reuse a successful report from a prior run.
  await rm(reportPath, { force: true })
  const checks = {}

  for (const name of CHECKS) {
    try {
      const result = await runCheck(name)
      const exitCode = Number.isInteger(result?.exitCode) ? result.exitCode : 1
      checks[name] = {
        status: exitCode === 0 ? 'passed' : 'failed',
        exitCode,
        ...(result?.error ? { error: result.error } : {}),
      }
    } catch (error) {
      checks[name] = { status: 'failed', exitCode: 1, error: error.message }
    }
  }

  const overall = CHECKS.every((name) => checks[name].exitCode === 0) ? 'passed' : 'failed'
  const report = buildReport({ diagnostics: [], overall, checks })
  await writeReport(report, { reportPath })
  return { report, exitCode: overall === 'passed' ? 0 : 1 }
}

async function main() {
  const result = await runGate()
  process.exitCode = result.exitCode
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main()
}
