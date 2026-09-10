/**
 * Production bundle gate: ensure test-only network mocking does not end up in
 * the production build.
 *
 * Scans JS/CSS/HTML and source maps.
 */
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const distDir = path.resolve(__dirname, '../dist')

const forbiddenPatterns = [
  /mockServiceWorker/i,
  /msw[/\\]/i,
]

const forbiddenFilenameFragments = [
  'mockServiceWorker',
]

function walk(dir) {
  const entries = []
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const fullPath = path.join(dir, entry.name)
    if (entry.isDirectory()) {
      entries.push(...walk(fullPath))
    } else if (
      entry.isFile() &&
      (entry.name.endsWith('.js') ||
        entry.name.endsWith('.css') ||
        entry.name.endsWith('.html') ||
        entry.name.endsWith('.map'))
    ) {
      entries.push(fullPath)
    }
  }
  return entries
}

function main() {
  if (!fs.existsSync(distDir)) {
    console.error(`dist directory not found: ${distDir}`)
    process.exit(1)
  }

  const files = walk(distDir)
  const violations = []

  for (const file of files) {
    const relative = path.relative(distDir, file)
    const basename = path.basename(file)

    for (const fragment of forbiddenFilenameFragments) {
      if (basename.includes(fragment)) {
        violations.push({ file: relative, pattern: `filename:${fragment}` })
      }
    }

    const content = fs.readFileSync(file, 'utf-8')
    for (const pattern of forbiddenPatterns) {
      if (pattern.test(content)) {
        violations.push({ file: relative, pattern: pattern.source })
      }
    }
  }

  if (violations.length > 0) {
    console.error('Production bundle gate failed: forbidden content detected')
    for (const v of violations) {
      console.error(`  ${v.file}: ${v.pattern}`)
    }
    process.exit(1)
  }

  console.log('Production bundle gate passed')
}

main()
