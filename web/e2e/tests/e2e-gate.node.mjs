import assert from 'node:assert/strict'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { runGate } from '../../scripts/e2e-gate.mjs'

async function withReportPath(callback) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'labweaver-e2e-gate-'))
  const reportPath = path.join(directory, 'report.json')
  try {
    return await callback(reportPath)
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
}

function runner(exitCodes) {
  return async (name) => ({ exitCode: exitCodes[name] })
}

test('E1 gate reports every successful check', async () => {
  await withReportPath(async (reportPath) => {
    const result = await runGate({ reportPath, runCheck: runner({ verify: 0, contract: 0, list: 0 }) })
    assert.equal(result.exitCode, 0)
    assert.equal(result.report.overall, 'passed')
    assert.deepEqual(result.report.checks, {
      verify: { status: 'passed', exitCode: 0 },
      contract: { status: 'passed', exitCode: 0 },
      list: { status: 'passed', exitCode: 0 },
    })
  })
})

test('E1 gate replaces an old passed report when contract fails', async () => {
  await withReportPath(async (reportPath) => {
    await writeFile(reportPath, '{"overall":"passed"}\n', 'utf8')
    const result = await runGate({ reportPath, runCheck: runner({ verify: 0, contract: 7, list: 0 }) })
    const report = JSON.parse(await readFile(reportPath, 'utf8'))
    assert.equal(result.exitCode, 1)
    assert.equal(report.overall, 'failed')
    assert.deepEqual(report.checks.contract, { status: 'failed', exitCode: 7 })
  })
})

test('E1 gate reports a list failure instead of retaining success', async () => {
  await withReportPath(async (reportPath) => {
    const result = await runGate({ reportPath, runCheck: runner({ verify: 0, contract: 0, list: 9 }) })
    assert.equal(result.exitCode, 1)
    assert.equal(result.report.overall, 'failed')
    assert.deepEqual(result.report.checks.list, { status: 'failed', exitCode: 9 })
  })
})
