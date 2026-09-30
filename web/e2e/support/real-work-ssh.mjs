import { createHash, randomUUID } from 'node:crypto'
import { chmod, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { spawn } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { performance } from 'node:perf_hooks'

const HOST_KEY_FINGERPRINT = /^SHA256:[A-Za-z0-9+/]{43}$/
const SSH_ALIAS = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/
const SSH_HOSTNAME = /^[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?$/
const VM_WORKSPACE_FILE = /^workspace(?:\/[A-Za-z0-9_-][A-Za-z0-9._-]*)+$/
const MAX_PROCESS_OUTPUT_BYTES = 1024 * 1024
export const VM_CUDA_PROBE_PTX_TARGET = 'sm_60'

const CUDA_DRIVER_PROBE = String.raw`import ctypes
import sys

PTX = b"""\
.version 6.0
.target ${VM_CUDA_PROBE_PTX_TARGET}
.address_size 64

.visible .entry reduce_stats(
    .param .u64 output
)
{
    .reg .pred %p;
    .reg .b32 %r<8>;
    .reg .b64 %rd<4>;
    ld.param.u64 %rd0, [output];
    mov.u32 %r0, %tid.x;
    mov.u32 %r1, %ctaid.x;
    mov.u32 %r5, %ntid.x;
    mad.lo.u32 %r2, %r1, %r5, %r0;
    setp.lt.u32 %p, %r2, 256;
    @!%p bra DONE;
    atom.global.add.u32 %r3, [%rd0], %r2;
    add.u64 %rd1, %rd0, 4;
    atom.global.add.u32 %r4, [%rd1], 1;
    add.u64 %rd2, %rd0, 8;
    atom.global.max.u32 %r5, [%rd2], %r2;
DONE:
    ret;
}
"""

def bind(driver, name, arguments):
    function = getattr(driver, name)
    function.argtypes = arguments
    function.restype = ctypes.c_int
    return function

def check(code, name):
    if code != 0:
        raise RuntimeError(f"CUDA_DRIVER_CALL_FAILED:{name}:{code}")

try:
    driver = ctypes.CDLL("libcuda.so.1")
except OSError:
    print("CUDA_DRIVER_LIBRARY_UNAVAILABLE", file=sys.stderr)
    raise SystemExit(70)

cu_init = bind(driver, "cuInit", [ctypes.c_uint])
cu_device_get = bind(driver, "cuDeviceGet", [ctypes.POINTER(ctypes.c_int), ctypes.c_int])
cu_ctx_create = bind(driver, "cuCtxCreate_v2", [ctypes.POINTER(ctypes.c_void_p), ctypes.c_uint, ctypes.c_int])
cu_ctx_destroy = bind(driver, "cuCtxDestroy_v2", [ctypes.c_void_p])
cu_module_load = bind(driver, "cuModuleLoadData", [ctypes.POINTER(ctypes.c_void_p), ctypes.c_void_p])
cu_module_get_function = bind(driver, "cuModuleGetFunction", [ctypes.POINTER(ctypes.c_void_p), ctypes.c_void_p, ctypes.c_char_p])
cu_module_unload = bind(driver, "cuModuleUnload", [ctypes.c_void_p])
cu_mem_alloc = bind(driver, "cuMemAlloc_v2", [ctypes.POINTER(ctypes.c_uint64), ctypes.c_size_t])
cu_mem_free = bind(driver, "cuMemFree_v2", [ctypes.c_uint64])
cu_memcpy_htod = bind(driver, "cuMemcpyHtoD_v2", [ctypes.c_uint64, ctypes.c_void_p, ctypes.c_size_t])
cu_memcpy_dtoh = bind(driver, "cuMemcpyDtoH_v2", [ctypes.c_void_p, ctypes.c_uint64, ctypes.c_size_t])
cu_launch = bind(driver, "cuLaunchKernel", [
    ctypes.c_void_p,
    ctypes.c_uint, ctypes.c_uint, ctypes.c_uint,
    ctypes.c_uint, ctypes.c_uint, ctypes.c_uint,
    ctypes.c_uint, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p,
])
cu_synchronize = bind(driver, "cuCtxSynchronize", [])

context = ctypes.c_void_p()
module = ctypes.c_void_p()
device_output = ctypes.c_uint64()
try:
    check(cu_init(0), "cuInit")
    device = ctypes.c_int()
    check(cu_device_get(ctypes.byref(device), 0), "cuDeviceGet")
    check(cu_ctx_create(ctypes.byref(context), 0, device.value), "cuCtxCreate_v2")

    ptx_buffer = ctypes.create_string_buffer(PTX + b"\0")
    check(cu_module_load(ctypes.byref(module), ctypes.cast(ptx_buffer, ctypes.c_void_p)), "cuModuleLoadData")
    kernel = ctypes.c_void_p()
    check(cu_module_get_function(ctypes.byref(kernel), module, b"reduce_stats"), "cuModuleGetFunction")

    check(cu_mem_alloc(ctypes.byref(device_output), ctypes.sizeof(ctypes.c_uint32) * 3), "cuMemAlloc_v2")
    host_output = (ctypes.c_uint32 * 3)(0, 0, 0)
    check(cu_memcpy_htod(device_output.value, ctypes.cast(host_output, ctypes.c_void_p), ctypes.sizeof(host_output)), "cuMemcpyHtoD_v2")

    kernel_argument = ctypes.c_uint64(device_output.value)
    kernel_parameters = (ctypes.c_void_p * 1)(ctypes.cast(ctypes.byref(kernel_argument), ctypes.c_void_p).value)
    check(cu_launch(
        kernel, 8, 1, 1, 32, 1, 1, 0, None,
        ctypes.cast(kernel_parameters, ctypes.c_void_p), None,
    ), "cuLaunchKernel")
    check(cu_synchronize(), "cuCtxSynchronize")
    check(cu_memcpy_dtoh(ctypes.cast(host_output, ctypes.c_void_p), device_output.value, ctypes.sizeof(host_output)), "cuMemcpyDtoH_v2")

    total, count, maximum = (int(value) for value in host_output)
    print(f"LABWEAVER_CUDA_RESULT count={count} sum={total} max={maximum}")
    if (count, total, maximum) != (256, 32640, 255):
        raise RuntimeError("CUDA_DRIVER_RESULT_MISMATCH")
except SystemExit:
    raise
except BaseException as error:
    message = str(error)
    print(message if message.startswith("CUDA_DRIVER_") else "CUDA_DRIVER_PROBE_FAILED", file=sys.stderr)
    raise SystemExit(71)
finally:
    if device_output.value:
        cu_mem_free(device_output.value)
    if module.value:
        cu_module_unload(module)
    if context.value:
        cu_ctx_destroy(context)
`

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

async function runPinnedSsh(endpointGrant, identity, command, input = undefined) {
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
  const resultLine = output
    .split(/\r?\n/)
    .map((line) => line.trim())
    .find((line) => /^LABWEAVER_CUDA_RESULT\s/.test(line))
  const match = resultLine?.match(/^LABWEAVER_CUDA_RESULT count=(\d+) sum=(\d+) max=(\d+)$/)
  if (!match) throw new Error('WORK_VM_CUDA_RESULT_MISSING')
  const result = { count: Number(match[1]), sum: Number(match[2]), max: Number(match[3]) }
  if (result.count !== 256 || result.sum !== 32640 || result.max !== 255) {
    throw new Error('WORK_VM_CUDA_RESULT_MISMATCH')
  }
  return result
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
