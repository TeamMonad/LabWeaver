import { readdir, readFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const WEB_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const SCAN_ROOTS = ['e2e', 'scripts', 'playwright.config.mjs']
const IGNORED_PATH_PARTS = new Set(['node_modules', 'artifacts', 'test-results', 'playwright-report', '.auth'])
const SELF_PATH = 'scripts/fixed-sleep-check.mjs'
const FIXED_SLEEP_PATTERNS = [
  new RegExp(`page\\.waitFor${'Timeout'}\\s*\\(`),
  new RegExp(`set${'Timeout'}\\s*\\(`),
  /\bsleep\s*\(/,
  /\bStart-Sleep\b/,
  /\bThread\.sleep\s*\(/,
]

async function collectFiles(target) {
  const absolute = path.join(WEB_ROOT, target)
  const entries = await readdir(absolute, { withFileTypes: true }).catch(() => [])
  if (entries.length === 0) return [target]
  const files = []
  for (const entry of entries) {
    if (IGNORED_PATH_PARTS.has(entry.name)) continue
    const relative = path.join(target, entry.name)
    if (entry.isDirectory()) files.push(...await collectFiles(relative))
    if (entry.isFile()) files.push(relative)
  }
  return files
}

export async function findFixedSleeps() {
  const files = (await Promise.all(SCAN_ROOTS.map(collectFiles))).flat()
  const findings = []
  for (const relative of files) {
    const normalized = relative.split(path.sep).join('/')
    if (normalized === SELF_PATH) continue
    const contents = await readFile(path.join(WEB_ROOT, relative), 'utf8').catch(() => '')
    for (const [index, line] of contents.split(/\r?\n/).entries()) {
      if (FIXED_SLEEP_PATTERNS.some((pattern) => pattern.test(line))) {
        findings.push(`${normalized}:${index + 1}`)
      }
    }
  }
  return findings
}

async function main() {
  const findings = await findFixedSleeps()
  if (findings.length > 0) {
    console.error('PW_FIXED_SLEEP_DETECTED')
    for (const finding of findings) console.error(finding)
    process.exitCode = 1
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main()
}
