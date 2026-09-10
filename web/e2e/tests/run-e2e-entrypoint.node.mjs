import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import {
  RUNTIME_PROJECTS,
  playwrightArguments,
  runE2e,
} from '../../scripts/run-e2e.mjs'

const WEB_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const ENTRYPOINT = path.join(WEB_ROOT, 'scripts', 'run-e2e.mjs')

function invokeEntrypoint({ baseUrl } = {}) {
  const environment = { ...process.env }
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
  }
}

test('run-e2e validates the base URL before browser execution', () => {
  const result = invokeEntrypoint()
  assert.equal(result.exitCode, 2)
  assert.match(result.stderr, /PW_BASE_URL_REQUIRED/)
})

test('runtime replay selects every configured authenticated role project', () => {
  assert.deepEqual(RUNTIME_PROJECTS, ['setup', 'teacher', 'student', 'platform-admin'])
  assert.deepEqual(playwrightArguments(), [
    'node_modules/@playwright/test/cli.js',
    'test',
    '--config=playwright.config.mjs',
    '--workers=1',
    '--project',
    'setup',
    '--project',
    'teacher',
    '--project',
    'student',
    '--project',
    'platform-admin',
  ])
})

test('run-e2e validates all configured role credentials before browser execution', () => {
  const result = invokeEntrypoint({ baseUrl: 'https://example.invalid' })
  assert.equal(result.exitCode, 2)
  assert.match(result.stderr, /PW_AUTH_CONFIGURATION_MISSING:LABWEAVER_TEACHER_USERNAME/)
  assert.match(result.stderr, /PW_AUTH_CONFIGURATION_MISSING:LABWEAVER_PLATFORM_ADMIN_USERNAME/)
  assert.doesNotMatch(result.stderr, /LABWEAVER_E2E_VM_ENVIRONMENT_ID/)
})

test('run-e2e executes Playwright after role credential validation succeeds', async () => {
  const temporaryDirectory = await mkdtemp(path.join(os.tmpdir(), 'labweaver-e2e-runtime-'))
  try {
    const passwordPath = path.join(temporaryDirectory, 'password')
    await writeFile(passwordPath, 'test-only-password\n', { encoding: 'utf8', mode: 0o600 })
    const environment = {
      ...process.env,
      LABWEAVER_BASE_URL: 'https://demo.lab.invalid',
      LABWEAVER_TEACHER_USERNAME: 'teacher',
      LABWEAVER_TEACHER_PASSWORD_FILE: passwordPath,
      LABWEAVER_STUDENT_USERNAME: 'student',
      LABWEAVER_STUDENT_PASSWORD_FILE: passwordPath,
      LABWEAVER_PLATFORM_ADMIN_USERNAME: 'platform-admin',
      LABWEAVER_PLATFORM_ADMIN_PASSWORD_FILE: passwordPath,
    }
    const result = await runE2e({
      environment,
      execute: async () => ({ exitCode: 0 }),
    })
    assert.equal(result.exitCode, 0)
    assert.deepEqual(result.diagnostics, [])
    assert.deepEqual(result.execution, { exitCode: 0 })
  } finally {
    await rm(temporaryDirectory, { recursive: true, force: true })
  }
})

test('run-e2e rejects invalid role password files before browser execution', async () => {
  const temporaryDirectory = await mkdtemp(path.join(os.tmpdir(), 'labweaver-e2e-password-'))
  try {
    const passwordPath = path.join(temporaryDirectory, 'password')
    await writeFile(passwordPath, '', { encoding: 'utf8', mode: 0o600 })
    let executed = false
    const result = await runE2e({
      environment: {
        ...process.env,
        LABWEAVER_BASE_URL: 'https://demo.lab.invalid',
        LABWEAVER_TEACHER_USERNAME: 'teacher',
        LABWEAVER_TEACHER_PASSWORD_FILE: passwordPath,
        LABWEAVER_STUDENT_USERNAME: 'student',
        LABWEAVER_STUDENT_PASSWORD_FILE: passwordPath,
        LABWEAVER_PLATFORM_ADMIN_USERNAME: 'platform-admin',
        LABWEAVER_PLATFORM_ADMIN_PASSWORD_FILE: passwordPath,
      },
      execute: async () => { executed = true; return { exitCode: 0 } },
    })
    assert.equal(result.exitCode, 2)
    assert.equal(executed, false)
    assert.deepEqual(result.diagnostics, [
      'PW_AUTH_PASSWORD_FILE_INVALID:LABWEAVER_TEACHER_PASSWORD_FILE',
      'PW_AUTH_PASSWORD_FILE_INVALID:LABWEAVER_STUDENT_PASSWORD_FILE',
      'PW_AUTH_PASSWORD_FILE_INVALID:LABWEAVER_PLATFORM_ADMIN_PASSWORD_FILE',
    ])
  } finally {
    await rm(temporaryDirectory, { recursive: true, force: true })
  }
})
