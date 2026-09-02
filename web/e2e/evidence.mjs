import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const REPOSITORY_ROOT = path.resolve(__dirname, '..', '..')
const WEB_ROOT = path.resolve(__dirname, '..')

function sourceCommit() {
  try {
    return execFileSync('git', ['rev-parse', 'HEAD'], { cwd: REPOSITORY_ROOT, encoding: 'utf8' }).trim()
  } catch {
    return undefined
  }
}

function fixtureManifestHash() {
  try {
    const manifestPath = path.join(WEB_ROOT, 'src', 'fixture', 'manifest.json')
    const canonical = JSON.stringify(JSON.parse(readFileSync(manifestPath, 'utf8')), Object.keys(JSON.parse(readFileSync(manifestPath, 'utf8'))).sort())
    return createHash('sha256').update(canonical).digest('hex').slice(0, 16)
  } catch {
    return undefined
  }
}

function browserVersion() {
  try {
    const pkg = JSON.parse(readFileSync(path.join(WEB_ROOT, 'node_modules', '@playwright', 'test', 'package.json'), 'utf8'))
    return pkg.version
  } catch {
    return undefined
  }
}

export function resolveEvidenceMetadata({ dataMode, evidenceLabel }) {
  const isFixture = dataMode === 'fixture'
  const commit = sourceCommit()
  const manifestHash = isFixture ? fixtureManifestHash() : undefined
  const browser = process.env.PW_BROWSER || 'chromium'
  const version = process.env.PW_BROWSER_VERSION || browserVersion()
  const viewport = { width: 1440, height: 900 }

  if (!commit) {
    throw new Error('PW_EVIDENCE_IDENTITY_MISSING: source commit is required')
  }
  if (isFixture && !manifestHash) {
    throw new Error('PW_EVIDENCE_IDENTITY_MISSING: fixture manifest hash is required')
  }
  if (!browser || !version) {
    throw new Error('PW_EVIDENCE_IDENTITY_MISSING: browser and version are required')
  }

  return {
    dataMode,
    evidenceLabel,
    sourceCommit: commit,
    fixtureManifestHash: manifestHash,
    browser,
    browserVersion: version,
    viewport,
  }
}
