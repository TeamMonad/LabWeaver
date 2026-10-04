import { createHash, randomUUID } from 'node:crypto'
import { chmod, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { spawn } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { performance } from 'node:perf_hooks'
import { CUDA_DRIVER_PROBE, parseCudaProbeResult } from './real-gpu.mjs'
export { CUDA_PROBE_PTX_TARGET as VM_CUDA_PROBE_PTX_TARGET } from './real-gpu.mjs'

const HOST_KEY_FINGERPRINT = /^SHA256:[A-Za-z0-9+/]{43}$/
const SSH_ALIAS = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/
const SSH_HOSTNAME = /^[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?$/
const VM_WORKSPACE_FILE = /^workspace(?:\/[A-Za-z0-9_-][A-Za-z0-9._-]*)+$/
const MAX_PROCESS_OUTPUT_BYTES = 1024 * 1024

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
    const startedAt = performance.now()
    let child
    try {
      child = spawn(program, args, {
        stdio: ['pipe', 'pipe', 'pipe'],
        windowsHide: true,
        timeout: timeoutMs,
        killSignal: 'SIGKILL',
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
      reject(new Error(`${outputCode}_START_FAILED`))
    })
    child.once('exit', () => {
      exitedAt = performance.now()
    })
    child.once('close', (code, signal) => {
      resolve({
        code,
        stdout: Buffer.concat(stdout).toString('utf8'),
        stderr: Buffer.concat(stderr).toString('utf8'),
        timedOut: signal === 'SIGKILL' && !outputExceeded && exitedAt !== null && exitedAt - startedAt >= timeoutMs,
        outputExceeded,
      })
    })
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

async function preparePinnedSsh(endpointGrant, identity) {
  const endpoint = validateSshEndpoint(endpointGrant)
  const scan = await runProcess('ssh-keyscan', [
    '-p', String(endpoint.port), '-T', '15', endpoint.hostname,
  ], { timeoutMs: 20_000, outputCode: 'WORK_SSH_GATEWAY_SCAN' })
  if (scan.timedOut) throw new Error('WORK_SSH_GATEWAY_SCAN_TIMEOUT')
  if (scan.outputExceeded) throw new Error('WORK_SSH_GATEWAY_SCAN_OUTPUT_LIMIT')
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

export async function runPinnedSsh(endpointGrant, identity, command, input = undefined) {
  const endpoint = await preparePinnedSsh(endpointGrant, identity)
  const result = await runProcess('ssh', [
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
    `${endpoint.alias}@${endpoint.hostname}`,
    command,
  ], { input, timeoutMs: 120_000, outputCode: 'WORK_SSH_COMMAND' })
  if (result.timedOut) throw new Error('WORK_SSH_COMMAND_TIMEOUT')
  if (result.outputExceeded) throw new Error('WORK_SSH_COMMAND_OUTPUT_LIMIT')
  if (result.code !== 0) {
    const guestDiagnostic = result.stderr
      .split(/\r?\n/)
      .map((line) => line.trim())
      .find((line) => /^(?:CUDA_DRIVER_[A-Z0-9_:.-]+|CUDA_DRIVER_LIBRARY_UNAVAILABLE)$/.test(line))
    throw new Error(guestDiagnostic ?? `WORK_SSH_COMMAND_FAILED:${result.code ?? 'signal'}`)
  }
  return result.stdout
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
      throw new Error(`WORK_VM_VGPU_LICENSE_NOT_GRANTED:${licenseStatus.toLowerCase()}`)
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

/** Confirm the guest reports an active vGPU license, independently of device visibility. */
export async function readRealWorkVmLicenseStatus(endpointGrant, identity) {
  const output = await runPinnedSsh(endpointGrant, identity, 'nvidia-smi -q')
  return parseRealWorkVmLicenseStatus(output)
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
