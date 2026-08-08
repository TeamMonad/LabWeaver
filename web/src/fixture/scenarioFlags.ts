/**
 * Deterministic demo scenario switches, driven by localStorage flags so both
 * Playwright (addInitScript / evaluate) and manual browser sessions can toggle
 * them without rebuilding. All flags live under the `fixture:` prefix and are
 * consumed where noted, keeping fixture behavior deterministic per page load.
 */

const PREFIX = 'fixture:'

function readFlag(name: string): string | null {
  try {
    return globalThis.localStorage?.getItem(`${PREFIX}${name}`) ?? null
  } catch {
    return null
  }
}

function writeFlag(name: string, value: string | null): void {
  try {
    if (value === null) globalThis.localStorage?.removeItem(`${PREFIX}${name}`)
    else globalThis.localStorage?.setItem(`${PREFIX}${name}`, value)
  } catch {
    // localStorage unavailable (non-browser context): flags are no-ops.
  }
}

/**
 * `fixture:demoDelayMs` — delay every fixture API response by N ms (capped at
 * 10 s) so loading states are observable. Read on each request.
 */
export function demoDelayMs(): number {
  const value = Number(readFlag('demoDelayMs'))
  return Number.isFinite(value) && value > 0 ? Math.min(value, 10_000) : 0
}

/**
 * `fixture:agentRunPollFailures` — the next N AgentRun poll requests fail with
 * a transient 500 so poll-gap recovery is demonstrable. Decremented per use.
 */
export function consumeAgentRunPollFailure(): boolean {
  const remaining = Number(readFlag('agentRunPollFailures'))
  if (!Number.isInteger(remaining) || remaining <= 0) return false
  writeFlag('agentRunPollFailures', String(remaining - 1))
  return true
}

/**
 * `fixture:rotatePolicy` — one-shot: the next active-policy read rotates the
 * stored policy to a new revision (consumed), so stale clients hit the
 * revision-mismatch conflict path on their next write.
 */
export function consumePolicyRotation(): boolean {
  if (readFlag('rotatePolicy') !== '1') return false
  writeFlag('rotatePolicy', null)
  return true
}

/**
 * `fixture:imageGate` — one-shot: mutate the next Environment candidate's
 * image evidence so a specific release gate fires. Allowed values:
 * `critical`, `high`, `wrong-digest`.
 */
export function consumeImageGateScenario(): string | null {
  const value = readFlag('imageGate')
  if (value === null) return null
  writeFlag('imageGate', null)
  return value
}

/**
 * `fixture:resourceRevisionConflict` — one-shot: the next Resource mutation
 * (request approve/reject/cancel/retry or Lease renew/revoke) first advances
 * the target revision as if a concurrent writer intervened, so the client's
 * revision fence fails with a stable 412 PRECONDITION_FAILED.
 */
export function consumeResourceRevisionConflict(): boolean {
  if (readFlag('resourceRevisionConflict') !== '1') return false
  writeFlag('resourceRevisionConflict', null)
  return true
}
