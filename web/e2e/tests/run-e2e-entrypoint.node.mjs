import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { runE2e } from '../../scripts/run-e2e.mjs'

const WEB_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const ENTRYPOINT = path.join(WEB_ROOT, 'scripts', 'run-e2e.mjs')

async function invokeEntrypoint({ baseUrl } = {}) {
  const temporaryDirectory = await mkdtemp(path.join(os.tmpdir(), 'labweaver-e2e-entrypoint-'))
  const reportPath = path.join(temporaryDirectory, 'report.json')
  try {
    const environment = { ...process.env, LABWEAVER_E2E_REPORT_PATH: reportPath }
    if (baseUrl === undefined) delete environment.LABWEAVER_BASE_URL
    else environment.LABWEAVER_BASE_URL = baseUrl
    const result = spawnSync(process.execPath, [ENTRYPOINT], {
      cwd: WEB_ROOT,
      env: environment,
      encoding: 'utf8',
    })
    return {
      exitCode: result.status,
      stderr: result.stderr,
      report: JSON.parse(await readFile(reportPath, 'utf8')),
      temporaryDirectory,
    }
  } finally {
    await rm(temporaryDirectory, { recursive: true, force: true })
  }
}

test('run-e2e entrypoint blocks without a base URL and cleans its isolated report directory', async () => {
  const result = await invokeEntrypoint()
  assert.equal(result.exitCode, 2)
  assert.match(result.stderr, /PW_BASE_URL_REQUIRED/)
  assert.equal(result.report.overall, 'blocked')
  assert.deepEqual(result.report.diagnostics, ['PW_BASE_URL_REQUIRED'])
  await assert.rejects(stat(result.temporaryDirectory))
})

test('run-e2e entrypoint blocks placeholder URLs without browser or network execution', async () => {
  const result = await invokeEntrypoint({ baseUrl: 'https://example.invalid' })
  assert.equal(result.exitCode, 2)
  assert.match(result.stderr, /PW_AUTH_CONFIGURATION_MISSING:LABWEAVER_TEACHER_USERNAME/)
  assert.match(result.stderr, /PW_AUTH_CONFIGURATION_MISSING:LABWEAVER_E2E_VM_ENVIRONMENT_ID/)
  assert.equal(result.report.overall, 'blocked')
  assert.equal(result.report.runtime_e2e, 'not_executed')
  await assert.rejects(stat(result.temporaryDirectory))
})

test('run-e2e records executed E3 browser evidence only after a successful runtime', async () => {
  const temporaryDirectory = await mkdtemp(path.join(os.tmpdir(), 'labweaver-e2e-runtime-'))
  try {
    const passwordPath = path.join(temporaryDirectory, 'password')
    const reportPath = path.join(temporaryDirectory, 'report.json')
    await writeFile(passwordPath, 'test-only-password\n', { encoding: 'utf8', mode: 0o600 })
    const environment = {
      ...process.env,
      LABWEAVER_BASE_URL: 'https://demo.lab.invalid',
      LABWEAVER_TEACHER_USERNAME: 'teacher',
      LABWEAVER_TEACHER_PASSWORD_FILE: passwordPath,
      LABWEAVER_STUDENT_USERNAME: 'student',
      LABWEAVER_STUDENT_PASSWORD_FILE: passwordPath,
      LABWEAVER_E2E_AGENT_RUN_ID: '01999999-9999-7999-8999-999999999999',
      LABWEAVER_E2E_CONTAINER_ENVIRONMENT_ID: '01999999-9999-7999-8999-999999999998',
      LABWEAVER_E2E_VM_ENVIRONMENT_ID: '01999999-9999-7999-8999-999999999997',
      LABWEAVER_E2E_REPORT_PATH: reportPath,
    }
    const result = await runE2e({
      environment,
      execute: async () => ({ exitCode: 0 }),
    })
    assert.equal(result.exitCode, 0)
    assert.equal(result.report.overall, 'passed')
    assert.equal(result.report.runtime_e2e, 'executed')
    assert.equal(result.report.evidenceLevel, 'E3')
    assert.deepEqual(result.report.checks.playwright, { status: 'passed', exitCode: 0 })
    assert.deepEqual(JSON.parse(await readFile(reportPath, 'utf8')), result.report)
  } finally {
    await rm(temporaryDirectory, { recursive: true, force: true })
  }
})
