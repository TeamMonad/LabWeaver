/**
 * Fixture manifest。
 *
 * 记录当前 fixture 覆盖的场景、固定 clock/seed、schema 版本，
 * 用于在测试报告、截图、Trace 中做可追踪的 identity 标注。
 */

export interface FixtureManifest {
  schemaVersion: 'fixture.labweaver.io/v1'
  seed: number
  epoch: string
  scenarios: string[]
  handlers: string[]
}

export const fixtureManifest: FixtureManifest = {
  schemaVersion: 'fixture.labweaver.io/v1',
  seed: 20260711,
  epoch: '2026-07-11T10:00:00.000Z',
  scenarios: [
    'ssh-keys:list',
    'ssh-keys:create',
    'ssh-keys:delete',
    'auth:unauthenticated',
    'headers:idempotency-key-missing',
    'headers:if-match-missing',
  ],
  handlers: ['sshKeys'],
}

async function digestHex(input: string): Promise<string> {
  const encoder = new TextEncoder()
  const data = encoder.encode(input)
  const buffer = await crypto.subtle.digest('SHA-256', data)
  const bytes = Array.from(new Uint8Array(buffer))
  return bytes.map((b) => b.toString(16).padStart(2, '0')).join('')
}

export async function computeManifestHash(manifest: FixtureManifest): Promise<string> {
  const canonical = JSON.stringify(manifest, Object.keys(manifest).sort())
  const full = await digestHex(canonical)
  return full.slice(0, 16)
}
