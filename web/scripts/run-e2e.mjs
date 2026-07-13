import { validateConfiguration, buildReport, writeReport } from './verify-config.mjs'

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

const { diagnostics } = await validateConfiguration({
  requirementsBaselineHead: process.env.PW_REQUIREMENTS_BASELINE_HEAD,
})
const baseUrl = process.env.LABWEAVER_BASE_URL
if (!baseUrl) fail('PW_BASE_URL_REQUIRED', diagnostics)
else if (!isHttpUrl(baseUrl)) fail('PW_BASE_URL_INVALID', diagnostics)

if (diagnostics.length === 0) {
  fail('PW_AUTH_SETUP_NOT_IMPLEMENTED', diagnostics)
  fail('PW_NO_RUNTIME_TESTS', diagnostics)
}

await writeReport(buildReport({ diagnostics, overall: 'blocked' }))
for (const code of diagnostics) console.error(code)
process.exitCode = 2
