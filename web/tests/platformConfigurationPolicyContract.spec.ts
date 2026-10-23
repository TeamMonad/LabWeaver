import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'
import type { PlatformConfigurationDesired } from '@/generated/contracts'
import { unsupportedPolicyFields, withoutUnsupportedPolicy } from '@/utils/platformConfiguration'

/**
 * The console mirrors the deployment path's fail-closed policy rules until Control validates them
 * (CONTRACT_GAP on #203). This test reads the two real rule sources, so adding or removing a rule
 * in the Bundle tool or the Helm helper fails here until the console follows.
 */
function repoFile(relative: string): string {
  // Vitest runs with web/ as the working directory (pnpm --dir web test).
  return readFileSync(resolve(process.cwd(), '..', relative), 'utf8')
}

function bundleToolRules(): string[] {
  const source = repoFile('tools/deployment_bundle.py')
  const start = source.indexOf('def _configuration_policy(')
  const end = source.indexOf('\ndef ', start + 1)
  expect(start, '_configuration_policy not found in tools/deployment_bundle.py').toBeGreaterThan(-1)
  const body = source.slice(start, end)
  expect(body).toContain('LW_PLATFORM_CONFIGURATION_POLICY_UNSUPPORTED')
  return [...new Set([...body.matchAll(/\b((?:runtimePolicy|builderPolicy)\.\w+)/g)].map((match) => match[1]))].sort()
}

function helmRules(): string[] {
  const source = repoFile('deploy/helm/labweaver/templates/_helpers.tpl')
  return [...new Set([...source.matchAll(/platformConfigurationPolicy\.((?:runtimePolicy|builderPolicy)\.\w+) has no execution-side consumer/g)].map((match) => match[1]))].sort()
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

describe('platform configuration fail-closed rules match the deployment path', () => {
  it('flags exactly the fields the Bundle tool and the Helm helper reject', () => {
    const consoleRules = unsupportedPolicyFields(violatingEverything()).map((field) => field.path).sort()
    const bundle = bundleToolRules()
    expect(bundle.length).toBeGreaterThan(0)
    expect(consoleRules).toEqual(bundle)
    expect(consoleRules).toEqual(helmRules())
  })

  it('resets to a configuration both rule sources accept', () => {
    expect(unsupportedPolicyFields(withoutUnsupportedPolicy(violatingEverything()))).toEqual([])
  })
})
