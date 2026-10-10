import { createHash, randomUUID } from 'node:crypto'
import { chmod, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { spawn } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { performance } from 'node:perf_hooks'
import { expect } from '@playwright/test'
import { CUDA_DRIVER_PROBE, parseCudaProbeResult } from './real-gpu.mjs'
export { CUDA_PROBE_PTX_TARGET as VM_CUDA_PROBE_PTX_TARGET } from './real-gpu.mjs'

const HOST_KEY_FINGERPRINT = /^SHA256:[A-Za-z0-9+/]{43}$/
const SSH_ALIAS = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/
const SSH_HOSTNAME = /^[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?$/
const VM_WORKSPACE_FILE = /^workspace(?:\/[A-Za-z0-9_-][A-Za-z0-9._-]*)+$/
const MAX_PROCESS_OUTPUT_BYTES = 1024 * 1024
const SAFE_GATEWAY_STAGES = new Set([
  'GATEWAY_CONFIGURATION',
  'GATEWAY_INPUT',
  'GATEWAY_ACCESS_AUTHORITY',
  'GATEWAY_TARGET_SESSION',
  'AUTHORIZED_KEYS_LOCAL_USER',
  'AUTHORIZED_KEYS_PRESENTED_KEY',
  'AUTHORIZED_KEYS_CONNECTION',
  'AUTHORIZED_KEYS_EXTRA_ARGUMENT',
  'AUTHORIZED_KEYS_SOURCE_ADDRESS',
  'AUTHORIZED_KEYS_CONNECTION_ID',
  'AUTHORIZED_KEYS_KEY_PARSE',
  'AUTHORIZED_KEYS_TIMESTAMP',
  'AUTHORIZED_KEYS_KEY_SERIALIZE',
])
const SAFE_CONFIG_PROBE_GUEST_FAILURES = new Set([
  'CONFIG_PROBE_PYTHON_UNSUPPORTED',
  'CONFIG_PROBE_GUEST_WORKSPACE_MISMATCH',
  'CONFIG_PROBE_OPENSSH_MISSING',
  'CONFIG_PROBE_SSH_INACTIVE',
])
const SAFE_GUEST_MODULES = new Set(['apt'])
export const REAL_WORK_VM_LICENSE_DEADLINE_MS = 180_000
const REAL_WORK_VM_LICENSE_RETRY_DELAY_MS = 5_000

function opensshFingerprint(keyType, encodedKey) {
  if (!/^[A-Za-z0-9@._+-]+$/.test(keyType) || !/^[A-Za-z0-9+/]+={0,2}$/.test(encodedKey)) {
    throw new Error('WORK_SSH_KEY_FORMAT_INVALID')
  }
  const keyBytes = Buffer.from(encodedKey, 'base64')
  if (keyBytes.length < 8 || keyBytes.toString('base64').replace(/=+$/, '') !== encodedKey.replace(/=+$/, '')) {
    throw new Error('WORK_SSH_KEY_FORMAT_INVALID')
  }
  return `SHA256:${createHash('sha256').update(keyBytes).digest('base64').replace(/=+$/, '')}`
}

function parseOpenSshKeyLine(line) {
  const fields = line.trim().split(/\s+/)
  if (fields.length < 2) throw new Error('WORK_SSH_KEY_FORMAT_INVALID')
  return { keyType: fields[0], encodedKey: fields[1] }
}

export function openSshPublicKeyFingerprint(publicKeyOpenssh) {
  const { keyType, encodedKey } = parseOpenSshKeyLine(publicKeyOpenssh)
  return opensshFingerprint(keyType, encodedKey)
}

export async function runProcess(program, args, {
  input,
  timeoutMs,
  outputCode,
  outputLimitBytes = MAX_PROCESS_OUTPUT_BYTES,
}) {
  return await new Promise((resolve, reject) => {
    let child
    try {
      child = spawn(program, args, {
        stdio: ['pipe', 'pipe', 'pipe'],
        windowsHide: true,
      })
    } catch {
      reject(new Error(`${outputCode}_START_FAILED`))
      return
    }
    const stdout = []
    const stderr = []
    let outputBytes = 0
    let outputExceeded = false
    let exitedAt = null
    let deadlineReached = false
    const deadlineSignal = AbortSignal.timeout(timeoutMs)
    const onDeadline = () => {
      if (exitedAt !== null || outputExceeded) return
      deadlineReached = true
      child.kill('SIGKILL')
    }
    const clearDeadlineListener = () => deadlineSignal.removeEventListener('abort', onDeadline)

    const collect = (chunks) => (chunk) => {
      if (outputExceeded) return
      outputBytes += chunk.length
      if (outputBytes > outputLimitBytes) {
        outputExceeded = true
        child.kill('SIGKILL')
        return
      }
      chunks.push(chunk)
    }
    child.stdout.on('data', collect(stdout))
    child.stderr.on('data', collect(stderr))
    child.once('error', () => {
      clearDeadlineListener()
      reject(new Error(`${outputCode}_START_FAILED`))
    })
    child.once('exit', () => {
      exitedAt = performance.now()
    })
    child.once('close', (code, signal) => {
      clearDeadlineListener()
      resolve({
        code,
        signal,
        stdout: Buffer.concat(stdout).toString('utf8'),
        stderr: Buffer.concat(stderr).toString('utf8'),
        timedOut: deadlineReached && !outputExceeded,
        outputExceeded,
      })
    })
    deadlineSignal.addEventListener('abort', onDeadline, { once: true })
    if (input === undefined) child.stdin.end()
    else child.stdin.end(input)
  })
}

function validateSshEndpoint(endpointGrant) {
  const hostname = endpointGrant?.sshGatewayHostname
  const port = endpointGrant?.sshGatewayPort
  const alias = endpointGrant?.alias
  const fingerprint = endpointGrant?.sshGatewayHostKeyFingerprint
  if (
    endpointGrant?.protocol !== 'ssh'
    || endpointGrant?.health !== 'healthy'
    || typeof hostname !== 'string'
    || !SSH_HOSTNAME.test(hostname)
    || typeof alias !== 'string'
    || !SSH_ALIAS.test(alias)
    || port !== 2222
    || typeof fingerprint !== 'string'
    || !HOST_KEY_FINGERPRINT.test(fingerprint)
  ) {
    throw new Error('WORK_SSH_ENDPOINT_GRANT_INVALID')
  }
  return { hostname, port, alias, fingerprint }
}

async function preparePinnedSsh(endpointGrant, identity, { deadlineAt = null } = {}) {
  const endpoint = validateSshEndpoint(endpointGrant)
  const scanTimeoutMs = deadlineAt === null
    ? 20_000
    : Math.max(1, Math.min(20_000, Math.ceil(deadlineAt - performance.now())))
  const scan = await runProcess('ssh-keyscan', [
    '-p', String(endpoint.port), '-T', '15', endpoint.hostname,
  ], { timeoutMs: scanTimeoutMs, outputCode: 'WORK_SSH_GATEWAY_SCAN' })
  if (scan.timedOut) throw new Error('WORK_SSH_GATEWAY_SCAN_TIMEOUT')
  if (scan.outputExceeded) throw new Error('WORK_SSH_GATEWAY_SCAN_OUTPUT_LIMIT')
  if (deadlineAt !== null && performance.now() >= deadlineAt) {
    throw new Error('WORK_SSH_COMMAND_TIMEOUT')
  }
  const knownHost = `[${endpoint.hostname}]:${endpoint.port}`
  const matchingKeys = scan.stdout
    .split(/\r?\n/)
    .filter((line) => line && !line.startsWith('#'))
    .map((line) => {
      try {
        const fields = line.trim().split(/\s+/)
        if (fields.length < 3) return null
        return parseOpenSshKeyLine(`${fields[1]} ${fields[2]}`)
      } catch {
        return null
      }
    })
    .filter(Boolean)
    .filter(({ keyType, encodedKey }) => opensshFingerprint(keyType, encodedKey) === endpoint.fingerprint)
  if (matchingKeys.length === 0) {
    if (scan.code !== 0 && scan.stdout.trim() === '') throw new Error('WORK_SSH_GATEWAY_HOST_KEY_UNAVAILABLE')
    throw new Error('WORK_SSH_GATEWAY_HOST_KEY_FINGERPRINT_MISMATCH')
  }

  const knownHostsPath = join(identity.directory, `known_hosts_${randomUUID()}`)
  const sshConfigPath = join(identity.directory, `ssh_config_${randomUUID()}`)
  const knownHosts = matchingKeys
    .map(({ keyType, encodedKey }) => `${knownHost} ${keyType} ${encodedKey}`)
    .join('\n') + '\n'
  await writeFile(knownHostsPath, knownHosts, { encoding: 'utf8', mode: 0o600 })
  await writeFile(sshConfigPath, '', { encoding: 'utf8', mode: 0o600 })
  await chmod(knownHostsPath, 0o600)
  await chmod(sshConfigPath, 0o600)
  return { ...endpoint, knownHostsPath, sshConfigPath }
}

export function pinnedSshArgs(endpoint, identity, command) {
  return [
    '-F', endpoint.sshConfigPath,
    '-T',
    '-p', String(endpoint.port),
    '-i', identity.privateKeyPath,
    '-o', 'BatchMode=yes',
    '-o', 'IdentitiesOnly=yes',
    '-o', 'PreferredAuthentications=publickey',
    '-o', 'PasswordAuthentication=no',
    '-o', 'KbdInteractiveAuthentication=no',
    '-o', 'StrictHostKeyChecking=yes',
    '-o', `UserKnownHostsFile=${endpoint.knownHostsPath}`,
    '-o', `GlobalKnownHostsFile=${process.platform === 'win32' ? 'NUL' : '/dev/null'}`,
    '-o', 'UpdateHostKeys=no',
    '-o', 'VerifyHostKeyDNS=no',
    '-o', 'ForwardAgent=no',
    '-o', 'ClearAllForwardings=yes',
    '-o', 'ProxyCommand=none',
    '-o', 'ProxyJump=none',
    '-o', 'ConnectTimeout=20',
    `gateway@${endpoint.hostname}`,
    `connect ${endpoint.alias} -- ${command}`,
  ]
}

function sshExitStatusToken(code, signal) {
  if (Number.isInteger(code) && code >= 0 && code <= 255) return `EXIT_${code}`
  if (typeof signal === 'string' && /^[A-Za-z0-9]+$/.test(signal)) {
    return `SIGNAL_${signal.toUpperCase()}`
  }
  return 'EXIT_UNKNOWN'
}

function safeGatewayFailureToken(stderr) {
  const token = stderr.match(/\b(LW_GATEWAY_[A-Z0-9_]+)\b/i)?.[1]?.toUpperCase()
  if (!token) return null
  const stage = stderr.match(/\b(?:failure[_ ]stage|stage)\b["']?\s*[:=]\s*["']?([A-Za-z0-9_.-]+)/i)?.[1]
  const safeStage = stage
    ? stage.toUpperCase().replace(/[.-]/g, '_').replace(/[^A-Z0-9_]/g, '')
    : ''
  return SAFE_GATEWAY_STAGES.has(safeStage) ? `${token}_STAGE_${safeStage}` : token
}

export function classifyPinnedSshCommandFailure({ code, signal, stderr = '' }) {
  const exitStatus = sshExitStatusToken(code, signal)
  const gatewayToken = safeGatewayFailureToken(stderr)
  let specificFailure = null
  if (/Permission denied \(publickey\)/i.test(stderr)) {
    specificFailure = 'PERMISSION_DENIED_PUBLICKEY'
  } else if (/Host key verification failed\.?/i.test(stderr)) {
    specificFailure = 'HOST_KEY_VERIFICATION_FAILED'
  } else if (/Connection refused/i.test(stderr)) {
    specificFailure = 'CONNECTION_REFUSED'
  } else if (/(?:Connection timed out|Operation timed out|connect to .* timed out)/i.test(stderr)) {
    specificFailure = 'CONNECTION_TIMEOUT'
  }
  if (specificFailure) {
    const gatewaySuffix = gatewayToken ? `_GATEWAY_${gatewayToken}` : ''
    return `WORK_SSH_COMMAND_${specificFailure}${gatewaySuffix}_${exitStatus}`
  }
  if (gatewayToken) return `WORK_SSH_COMMAND_${gatewayToken}_${exitStatus}`
  return `WORK_SSH_COMMAND_FAILED_${exitStatus}`
}

export function classifyPinnedSshCommandStage(command, input = undefined) {
  if (command === 'nvidia-smi -q') return 'GPU_LICENSE'
  if (command === 'python3 -' && input === CUDA_DRIVER_PROBE) return 'CUDA_PROBE'
  if (command === 'python3 -') return 'GUEST_SCRIPT'
  if (typeof command === 'string' && command.startsWith('cat -- "$HOME/workspace/')) return 'WORKSPACE_READ'
  if (command === 'printf WORK_SSH_OLD_ALIAS_ACCEPTED') return 'REVOKED_ACCESS_CHECK'
  return 'COMMAND'
}

export function classifyPinnedSshGuestFailure(stderr = '') {
  for (const line of String(stderr).split(/\r?\n/)) {
    const trimmed = line.trim()
    if (/^(?:CUDA_DRIVER_[A-Z0-9_:.-]+|CUDA_DRIVER_LIBRARY_UNAVAILABLE)$/.test(trimmed)) {
      return trimmed
    }
    const configProbeCode = trimmed.match(/\b(CONFIG_PROBE_[A-Z0-9_]+)\b/)?.[1]
    if (configProbeCode && SAFE_CONFIG_PROBE_GUEST_FAILURES.has(configProbeCode)) {
      return configProbeCode
    }
  }

  const missingModule = String(stderr)
    .match(/\b(?:ModuleNotFoundError|ImportError):\s+No module named\s+['"]?([A-Za-z0-9_]+)['"]?/i)?.[1]
    ?.toLowerCase()
  if (missingModule && SAFE_GUEST_MODULES.has(missingModule)) {
    return `WORK_SSH_GUEST_MODULE_MISSING_${missingModule.toUpperCase()}`
  }
  return null
}

export async function runPinnedSsh(endpointGrant, identity, command, input = undefined, { timeoutMs = 120_000 } = {}) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1) throw new Error('WORK_SSH_COMMAND_TIMEOUT_INVALID')
  const deadlineAt = performance.now() + timeoutMs
  const endpoint = await preparePinnedSsh(endpointGrant, identity, { deadlineAt })
  const commandStage = classifyPinnedSshCommandStage(command, input)
  const commandTimeoutMs = Math.max(1, Math.ceil(deadlineAt - performance.now()))
  const result = await runProcess('ssh', pinnedSshArgs(endpoint, identity, command), {
    input,
    timeoutMs: commandTimeoutMs,
    outputCode: 'WORK_SSH_COMMAND',
  })
  if (result.timedOut) throw new Error(`WORK_SSH_COMMAND_TIMEOUT_${commandStage}`)
  if (result.outputExceeded) throw new Error('WORK_SSH_COMMAND_OUTPUT_LIMIT')
  if (result.code !== 0) {
    const guestDiagnostic = classifyPinnedSshGuestFailure(result.stderr)
    throw new Error(guestDiagnostic ?? classifyPinnedSshCommandFailure(result))
  }
  return result.stdout
}

export function classifyPinnedSshSessionClose({ code, signal, stderr = '' }) {
  if (/(?:closed by remote host|connection reset|connection aborted|broken pipe|administratively prohibited)/i.test(stderr)) {
    return 'remote_terminated'
  }
  if (signal) return 'process_signaled'
  if (code === 0) return 'process_exited'
  return 'process_failed'
}

/**
 * Keep one pinned SSH connection open until the gateway closes it. The ready
 * marker proves the authenticated session reached the guest before a grant is
 * revoked; waitForClose() reports the actual child exit instead of treating a
 * timeout as revocation evidence.
 */
export async function openPinnedSshSession(endpointGrant, identity) {
  const endpoint = await preparePinnedSsh(endpointGrant, identity)
  const readyMarker = `LABWEAVER_SSH_SESSION_READY_${randomUUID()}`
  const command = `printf '%s\\n' '${readyMarker}'; while IFS= read -r line; do :; done`
  let child
  try {
    child = spawn('ssh', pinnedSshArgs(endpoint, identity, command), {
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    })
  } catch {
    throw new Error('WORK_SSH_SESSION_START_FAILED')
  }

  const stdout = []
  const stderr = []
  let outputBytes = 0
  let outputExceeded = false
  let ready = false
  let closed = false
  let closeResult = null
  let resolveReady
  let rejectReady
  let resolveClosed
  const readyPromise = new Promise((resolve, reject) => {
    resolveReady = resolve
    rejectReady = reject
  })
  const closePromise = new Promise((resolve) => {
    resolveClosed = resolve
  })
  const collect = (chunks) => (chunk) => {
    if (outputExceeded) return
    outputBytes += chunk.length
    if (outputBytes > MAX_PROCESS_OUTPUT_BYTES) {
      outputExceeded = true
      child.kill('SIGKILL')
      return
    }
    chunks.push(chunk)
  }
  const stdoutListener = collect(stdout)
  child.stdout.on('data', (chunk) => {
    stdoutListener(chunk)
    if (!ready && !outputExceeded && Buffer.concat(stdout).toString('utf8').includes(readyMarker)) {
      ready = true
      resolveReady()
    }
  })
  child.stderr.on('data', collect(stderr))
  child.stdin.on('error', () => {})
  child.once('error', () => {
    if (!ready) rejectReady(new Error('WORK_SSH_SESSION_START_FAILED'))
  })
  child.once('close', (code, signal) => {
    closed = true
    closeResult = {
      closed: true,
      code,
      signal,
      reasonCode: outputExceeded
        ? 'output_limit'
        : classifyPinnedSshSessionClose({
          code,
          signal,
          stderr: Buffer.concat(stderr).toString('utf8'),
        }),
    }
    if (!ready) rejectReady(new Error('WORK_SSH_SESSION_CONNECT_FAILED'))
    resolveClosed(closeResult)
  })

  try {
    const readyDeadline = AbortSignal.timeout(30_000)
    await Promise.race([
      readyPromise,
      new Promise((_, reject) => {
        const onAbort = () => reject(new Error('WORK_SSH_SESSION_CONNECT_TIMEOUT'))
        readyDeadline.addEventListener('abort', onAbort, { once: true })
        if (readyDeadline.aborted) onAbort()
      }),
    ])
  } catch (error) {
    if (!closed) child.kill('SIGKILL')
    await closePromise
    throw error
  }

  async function waitForClose(timeoutMs = 90_000) {
    if (closed) return closeResult
    const closeDeadline = AbortSignal.timeout(timeoutMs)
    return await Promise.race([
      closePromise,
      new Promise((_, reject) => {
        const onAbort = () => reject(new Error('WORK_SSH_SESSION_CLOSE_TIMEOUT'))
        closeDeadline.addEventListener('abort', onAbort, { once: true })
        if (closeDeadline.aborted) onAbort()
      }),
    ])
  }

  async function close() {
    if (closed) return closeResult
    try {
      child.stdin.end('exit\n')
    } catch {
      // The gateway may have already closed the pipe; closeResult captures it.
    }
    try {
      return await waitForClose(10_000)
    } catch {
      if (!closed) child.kill('SIGKILL')
      return await closePromise
    }
  }

  return {
    get closed() {
      return closed
    },
    waitForClose,
    close,
  }
}

/**
 * Generate a disposable client key locally. Only publicKeyOpenssh is intended
 * to be registered with the user's SSH-key page; the private key stays in this
 * mode-0700 temporary directory and is removed by cleanup().
 */
export async function createRealWorkSshIdentity() {
  const directory = await mkdtemp(join(tmpdir(), 'labweaver-work-ssh-'))
  const privateKeyPath = join(directory, 'id_ed25519')
  try {
    await chmod(directory, 0o700)
    const generated = await runProcess('ssh-keygen', [
      '-q', '-t', 'ed25519', '-N', '', '-C', 'labweaver-work-acceptance', '-f', privateKeyPath,
    ], { timeoutMs: 30_000, outputCode: 'WORK_SSH_KEYGEN' })
    if (generated.timedOut || generated.code !== 0) throw new Error('WORK_SSH_KEY_GENERATION_FAILED')
    const publicKeyOpenssh = (await readFile(`${privateKeyPath}.pub`, 'utf8')).trim()
    const { keyType, encodedKey } = parseOpenSshKeyLine(publicKeyOpenssh)
    if (keyType !== 'ssh-ed25519') throw new Error('WORK_SSH_KEY_ALGORITHM_INVALID')
    const fingerprintSha256 = opensshFingerprint(keyType, encodedKey)
    await chmod(privateKeyPath, 0o600)
    await chmod(`${privateKeyPath}.pub`, 0o600)
    return {
      directory,
      privateKeyPath,
      publicKeyOpenssh,
      fingerprintSha256,
      async cleanup() {
        await rm(directory, { recursive: true, force: true })
      },
    }
  } catch {
    await rm(directory, { recursive: true, force: true })
    throw new Error('WORK_SSH_KEY_GENERATION_FAILED')
  }
}

/** Run a real CUDA Driver API kernel inside the granted VM and verify readback. */
export async function runRealWorkVmCudaProbe(endpointGrant, identity) {
  const output = await runPinnedSsh(endpointGrant, identity, 'python3 -', CUDA_DRIVER_PROBE)
  return parseCudaProbeResult(output)
}

export function parseRealWorkVmLicenseStatus(output) {
  const sections = output.split(/(?=^GPU\s+\S+)/m)
    .filter((section) => /vGPU Software Licensed Product/i.test(section))
  if (sections.length === 0) throw new Error('WORK_VM_VGPU_LICENSE_STATUS_MISSING')
  const driverVersion = output.match(/^\s*Driver Version\s*:\s*(\S+)\s*$/im)?.[1]
  if (!driverVersion) throw new Error('WORK_VM_VGPU_DRIVER_VERSION_MISSING')
  const licenses = sections.map((section) => {
    const statusMatch = section.match(/^\s*License Status\s*:\s*([^()\r\n]+?)(?:\s*\(Expiry:\s*([^)]*)\))?\s*$/im)
    if (!statusMatch) throw new Error('WORK_VM_VGPU_LICENSE_STATUS_MISSING')
    const licenseStatus = statusMatch[1].trim()
    if (licenseStatus.toLowerCase() !== 'licensed') {
      const safeStatus = licenseStatus.toLowerCase().replace(/[^a-z0-9_-]+/g, '_').replace(/^_+|_+$/g, '').slice(0, 64)
      throw new Error(`WORK_VM_VGPU_LICENSE_NOT_GRANTED:${safeStatus || 'unknown'}`)
    }
    return { licenseStatus, expiry: statusMatch[2]?.trim() || null }
  })
  if (licenses.some((license) => license.licenseStatus !== licenses[0].licenseStatus)) {
    throw new Error('WORK_VM_VGPU_LICENSE_STATUS_INCONSISTENT')
  }
  const product = sections[0].match(/^\s*Product Name\s*:\s*(.+?)\s*$/im)?.[1]
  return {
    driverVersion,
    licenseStatus: licenses[0].licenseStatus,
    expiry: licenses[0].expiry,
    product: product ?? null,
    licensedGpuCount: licenses.length,
  }
}

function isLicenseNotGranted(error) {
  return error instanceof Error && /^WORK_VM_VGPU_LICENSE_NOT_GRANTED:[a-z0-9_-]+$/.test(error.message)
}

function licenseFailureStatus(error) {
  return error instanceof Error
    ? error.message.match(/^WORK_VM_VGPU_LICENSE_NOT_GRANTED:([a-z0-9_-]+)$/)?.[1] ?? 'unknown'
    : 'unknown'
}

/** Confirm the guest reports an active vGPU license, independently of device visibility. */
export async function readRealWorkVmLicenseStatus(
  endpointGrant,
  identity,
  {
    runSsh = runPinnedSsh,
    now = () => performance.now(),
    deadlineMs = REAL_WORK_VM_LICENSE_DEADLINE_MS,
    retryDelayMs = REAL_WORK_VM_LICENSE_RETRY_DELAY_MS,
  } = {},
) {
  if (!Number.isSafeInteger(deadlineMs) || deadlineMs < 1) throw new Error('WORK_VM_VGPU_LICENSE_DEADLINE_INVALID')
  if (!Number.isSafeInteger(retryDelayMs) || retryDelayMs < 1) throw new Error('WORK_VM_VGPU_LICENSE_RETRY_DELAY_INVALID')

  const deadlineAt = now() + deadlineMs
  let lastFailure = null
  let terminalError = null
  let licensedResult = null
  try {
    await expect.poll(
      async () => {
        const remaining = Math.max(1, Math.ceil(deadlineAt - now()))
        try {
          const output = await runSsh(endpointGrant, identity, 'nvidia-smi -q', undefined, { timeoutMs: remaining })
          licensedResult = parseRealWorkVmLicenseStatus(output)
          return 'licensed'
        } catch (error) {
          if (!isLicenseNotGranted(error)) {
            terminalError = error
            return 'terminal'
          }
          lastFailure = error
          return 'unlicensed'
        }
      },
      {
        timeout: deadlineMs,
        intervals: [retryDelayMs, Math.max(retryDelayMs, retryDelayMs * 2), Math.max(retryDelayMs, retryDelayMs * 4)],
      },
    ).toMatch(/^(licensed|terminal)$/)
  } catch (error) {
    if (terminalError) throw terminalError
    if (lastFailure) throw new Error(`WORK_VM_VGPU_LICENSE_NOT_GRANTED:${licenseFailureStatus(lastFailure)}`)
    throw error
  }

  if (terminalError) throw terminalError
  if (!licensedResult) throw new Error(`WORK_VM_VGPU_LICENSE_NOT_GRANTED:${licenseFailureStatus(lastFailure)}`)
  return licensedResult
}

/** Read a workspace file relative to the authorized SSH account's home. */
export async function readRealWorkVmWorkspaceFile(endpointGrant, identity, filePath) {
  const relativePath = realWorkVmWorkspaceRelativePath(filePath)
  return await runPinnedSsh(endpointGrant, identity, `cat -- "$HOME/${relativePath}"`)
}

/** Keep VM reads relative to the authorized SSH account's configured home. */
export function realWorkVmWorkspaceRelativePath(filePath) {
  if (typeof filePath !== 'string' || !VM_WORKSPACE_FILE.test(filePath)) {
    throw new Error('WORK_VM_WORKSPACE_PATH_INVALID')
  }
  return filePath
}
