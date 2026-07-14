import { mkdtemp, readFile, readdir, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'

const webRoot = resolve(import.meta.dirname, '..')
const checkedIn = join(webRoot, 'src/generated/contracts')
const temporary = await mkdtemp(join(tmpdir(), 'labweaver-contracts-'))

async function files(root, directory = root) {
  const output = []
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) output.push(...await files(root, path))
    else output.push(relative(root, path).replaceAll('\\', '/'))
  }
  return output.sort()
}

try {
  const packageRoot = dirname(fileURLToPath(import.meta.resolve('@hey-api/openapi-ts/package.json')))
  await promisify(execFile)(process.execPath, [
    join(packageRoot, 'bin/run.js'),
    '-f', 'openapi-ts.config.ts',
    '-o', temporary,
    '--silent',
  ], { cwd: webRoot })
  const expected = await files(checkedIn)
  const actual = await files(temporary)
  if (JSON.stringify(expected) !== JSON.stringify(actual)) {
    throw new Error(`LW_CONTRACT_TS_DRIFT: generated file set differs: expected ${expected.join(', ')}, actual ${actual.join(', ')}`)
  }
  for (const path of expected) {
    const [committed, generated] = await Promise.all([
      readFile(join(checkedIn, path)),
      readFile(join(temporary, path)),
    ])
    if (!committed.equals(generated)) {
      throw new Error(`LW_CONTRACT_TS_DRIFT: ${path} differs from OpenAPI`)
    }
  }
} finally {
  await rm(temporary, { recursive: true, force: true })
}
