import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'
import type { PlatformConfigurationDesired } from '@/generated/contracts'
import { unsupportedPolicyFields, withoutUnsupportedPolicy } from '@/utils/platformConfiguration'

/** The console mirrors the Rust-generated unsupported policy manifest and the deployment boundaries. */
function repoFile(relative: string): string {
  // Vitest runs with web/ as the working directory (pnpm --dir web test).
  return readFileSync(resolve(process.cwd(), '..', relative), 'utf8')
}

function manifestRules(): string[] {
  const manifest = JSON.parse(repoFile('schemas/contracts/v1/platform-configuration-unsupported-fields.json')) as {
    apiVersion: string
    unsupportedFields: string[]
  }
  expect(manifest.apiVersion).toBe('platform-configuration-policy/v1')
  return [...manifest.unsupportedFields].sort()
}

function bundleToolRules(fields: string[]): string[] {
  const source = repoFile('tools/deployment_bundle.py')
  const start = source.indexOf('def _configuration_policy(')
  const end = source.indexOf('\ndef ', start + 1)
  expect(start, '_configuration_policy not found in tools/deployment_bundle.py').toBeGreaterThan(-1)
  const body = source.slice(start, end)
  expect(body).toContain('LW_PLATFORM_CONFIGURATION_POLICY_UNSUPPORTED')
  for (const field of fields) expect(body).toContain(field)
  return fields
}

function helmRules(fields: string[]): string[] {
  const source = repoFile('deploy/helm/labweaver/templates/_helpers.tpl')
  for (const field of fields) expect(source).toContain(`platformConfigurationPolicy.${field}`)
  return fields
}

/** A configuration that violates every rule the console knows about. */
function violatingEverything(): PlatformConfigurationDesired {
  return {
    lifecycle: { readOnlyRetentionSeconds: 604800, detailedLogRetentionSeconds: 2592000, expiryReminderSeconds: [86400, 3600], preemptionGraceSeconds: 60 },
    runtimePolicy: {
      defaultContainerUid: 1000,
      allowedCapabilities: ['SYS_PTRACE'],
      preauthorizations: [{ scope: { kind: 'project', projectId: 'p-1' }, capabilities: ['SYS_PTRACE'], privileged: true }],
      privilegedAllowed: true,
      privilegedExecutionLocation: { kind: 'virtual_machine_guest', providerBinding: 'kubevirt' },
    },
    builderPolicy: { capabilities: ['SYS_ADMIN'], allowPrivilegeEscalation: true, entitlements: ['network.host'] },
  }
}

describe('platform configuration fail-closed rules match the Rust manifest', () => {
  it('flags exactly the generated unsupported fields in Bundle, Helm and frontend', () => {
    const manifest = manifestRules()
    const consoleRules = unsupportedPolicyFields(violatingEverything()).map((field) => field.path).sort()
    expect(consoleRules).toEqual(manifest)
    expect(bundleToolRules(manifest)).toEqual(manifest)
    expect(helmRules(manifest)).toEqual(manifest)
  })

  it('resets to a configuration accepted by the Rust manifest', () => {
    expect(unsupportedPolicyFields(withoutUnsupportedPolicy(violatingEverything()))).toEqual([])
  })
})
