import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

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
  assert.match(result.stderr, /PW_AUTH_SETUP_NOT_IMPLEMENTED/)
  assert.match(result.stderr, /PW_NO_RUNTIME_TESTS/)
  assert.equal(result.report.overall, 'blocked')
  assert.deepEqual(result.report.diagnostics, ['PW_AUTH_SETUP_NOT_IMPLEMENTED', 'PW_NO_RUNTIME_TESTS'])
  await assert.rejects(stat(result.temporaryDirectory))
})
