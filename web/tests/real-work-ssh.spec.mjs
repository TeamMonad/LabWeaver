import { describe, expect, it } from 'vitest'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  CUDA_DRIVER_PROBE,
  cudaProbeTerminalCommand,
  normalizeTerminalOutput,
  terminalCommandExit,
  terminalCommandFailure,
  terminalCommandLines,
  typeTerminalCommand,
} from '../e2e/support/real-gpu.mjs'
import {
  classifyPinnedSshSessionClose,
  classifyPinnedSshCommandFailure,
  classifyPinnedSshGuestFailure,
  openSshPublicKeyFingerprint,
  parseRealWorkVmLicenseStatus,
  pinnedSshArgs,
  realWorkVmWorkspaceRelativePath,
  readRealWorkVmLicenseStatus,
  runProcess,
  VM_CUDA_PROBE_PTX_TARGET,
} from '../e2e/support/real-work-ssh.mjs'

describe('real Work SSH helper', () => {
  it('uses the gateway relay account and connect protocol for interactive and guest commands', () => {
    const args = pinnedSshArgs(
      {
        hostname: 'gateway.example',
        port: 2222,
        alias: 'lw-abcdefghijklmnopqrst',
        knownHostsPath: 'known_hosts',
        sshConfigPath: 'ssh_config',
      },
      { privateKeyPath: 'id_ed25519' },
      'printf READY',
    )

    expect(args.at(-2)).toBe('gateway@gateway.example')
    expect(args.at(-1)).toBe('connect lw-abcdefghijklmnopqrst -- printf READY')
  })

  it('uses PTX supported by the current P40 and V100 worker GPUs', () => {
    expect(VM_CUDA_PROBE_PTX_TARGET).toBe('sm_60')
  })

  it('derives the OpenSSH SHA-256 fingerprint from the encoded key blob', () => {
    const publicKey = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA acceptance-test'
    expect(openSshPublicKeyFingerprint(publicKey)).toBe('SHA256:kmYcvdi2GkPeWxB6XLjrZB8JHsy2Hm8luHMFp9GMvqk')
  })

  it('rejects malformed key material before it can be pinned or registered', () => {
    expect(() => openSshPublicKeyFingerprint('ssh-ed25519 not-base64')).toThrow('WORK_SSH_KEY_FORMAT_INVALID')
    expect(() => openSshPublicKeyFingerprint('')).toThrow('WORK_SSH_KEY_FORMAT_INVALID')
  })

  it('requires an explicit licensed status and reports the guest driver version', () => {
    const status = parseRealWorkVmLicenseStatus(`==============NVSMI LOG==============

Timestamp : Wed Sep 30 09:00:00 2026
Driver Version : 580.126.09

GPU 00000000:00:00.0
    vGPU Software Licensed Product
        Product Name : NVIDIA RTX vWS
        License Status : Licensed (Expiry: 2026-10-30 09:00:00 GMT)
`)
    expect(status).toEqual({
      driverVersion: '580.126.09',
      licenseStatus: 'Licensed',
      expiry: '2026-10-30 09:00:00 GMT',
      product: 'NVIDIA RTX vWS',
      licensedGpuCount: 1,
    })
  })

  it('fails closed when any vGPU device license is missing, unlicensed, unknown, or denied', () => {
    const makeOutput = (licenseStatus) => `Driver Version : 580.126.09\nGPU 00000000:00:00.0\n    vGPU Software Licensed Product\n        License Status : ${licenseStatus}\n`
    expect(() => parseRealWorkVmLicenseStatus('Driver Version : 580.126.09\n')).toThrow('WORK_VM_VGPU_LICENSE_STATUS_MISSING')
    expect(() => parseRealWorkVmLicenseStatus(makeOutput('Unlicensed'))).toThrow('WORK_VM_VGPU_LICENSE_NOT_GRANTED:unlicensed')
    expect(() => parseRealWorkVmLicenseStatus(makeOutput('Unknown'))).toThrow('WORK_VM_VGPU_LICENSE_NOT_GRANTED:unknown')
    expect(() => parseRealWorkVmLicenseStatus(makeOutput('Failed'))).toThrow('WORK_VM_VGPU_LICENSE_NOT_GRANTED:failed')
    expect(() => parseRealWorkVmLicenseStatus(makeOutput('Licensed (Renewal pending)'))).toThrow('WORK_VM_VGPU_LICENSE_STATUS_MISSING')
  })

  it('checks every vGPU device section instead of accepting only the first licensed device', () => {
    const output = `Driver Version : 580.126.09
GPU 00000000:00:00.0
    vGPU Software Licensed Product
        Product Name : NVIDIA RTX vWS
        License Status : Licensed (Expiry: Never)

GPU 00000000:01:00.0
    vGPU Software Licensed Product
        Product Name : NVIDIA RTX vWS
        License Status : Unlicensed
`
    expect(() => parseRealWorkVmLicenseStatus(output)).toThrow('WORK_VM_VGPU_LICENSE_NOT_GRANTED:unlicensed')
  })

  it('retries an explicit unlicensed result until the same guest becomes licensed', async () => {
    const unlicensed = 'Driver Version : 580.159.03\nGPU 00000000:00:00.0\n    vGPU Software Licensed Product\n        License Status : Unlicensed\n'
    const licensed = 'Driver Version : 580.159.03\nGPU 00000000:00:00.0\n    vGPU Software Licensed Product\n        Product Name : NVIDIA RTX vWS\n        License Status : Licensed (Expiry: Never)\n'
    const calls = []
    const result = await readRealWorkVmLicenseStatus({}, {}, {
      deadlineMs: 100,
      retryDelayMs: 1,
      runSsh: async (...args) => {
        calls.push(args)
        return calls.length === 1 ? unlicensed : licensed
      },
    })
    expect(result).toMatchObject({ licenseStatus: 'Licensed', driverVersion: '580.159.03' })
    expect(calls).toHaveLength(2)
    expect(calls.every(([, , command]) => command === 'nvidia-smi -q')).toBe(true)
    expect(calls.every(([, , , , options]) => options.timeoutMs <= 100)).toBe(true)
  })

  it('keeps the last license status when the bounded wait expires', async () => {
    let clock = 0
    const calls = []
    const unlicensed = 'Driver Version : 580.159.03\nGPU 00000000:00:00.0\n    vGPU Software Licensed Product\n        License Status : Unlicensed\n'
    await expect(readRealWorkVmLicenseStatus({}, {}, {
      deadlineMs: 50,
      retryDelayMs: 10,
      now: () => clock,
      runSsh: async (...args) => {
        calls.push(args)
        clock += 3
        return unlicensed
      },
    })).rejects.toThrow('WORK_VM_VGPU_LICENSE_NOT_GRANTED:unlicensed')
    expect(calls.every(([, , command]) => command === 'nvidia-smi -q')).toBe(true)
    expect(calls.every(([, , , , options]) => options.timeoutMs <= 50)).toBe(true)
  })

  it('keeps the last license status when the final gateway scan reaches the deadline', async () => {
    let clock = 0
    const calls = []
    const unlicensed = 'Driver Version : 580.159.03\nGPU 00000000:00:00.0\n    vGPU Software Licensed Product\n        License Status : Unlicensed\n'
    await expect(readRealWorkVmLicenseStatus({}, {}, {
      deadlineMs: 50,
      retryDelayMs: 1,
      now: () => clock,
      runSsh: async (...args) => {
        calls.push(args)
        if (calls.length === 1) {
          clock = 49
          return unlicensed
        }
        clock = 50
        throw new Error('WORK_SSH_GATEWAY_SCAN_TIMEOUT')
      },
    })).rejects.toThrow('WORK_VM_VGPU_LICENSE_NOT_GRANTED:unlicensed')
    expect(calls).toHaveLength(2)
  })

  it('keeps a gateway scan failure before the deadline terminal', async () => {
    const calls = []
    await expect(readRealWorkVmLicenseStatus({}, {}, {
      deadlineMs: 100,
      runSsh: async (...args) => {
        calls.push(args)
        throw new Error('WORK_SSH_GATEWAY_SCAN_TIMEOUT')
      },
    })).rejects.toThrow('WORK_SSH_GATEWAY_SCAN_TIMEOUT')
    expect(calls).toHaveLength(1)
  })

  it('does not retry malformed license output or an SSH failure', async () => {
    const calls = []
    await expect(readRealWorkVmLicenseStatus({}, {}, {
      deadlineMs: 100,
      runSsh: async (...args) => {
        calls.push(args)
        return 'Driver Version : 580.159.03\n'
      },
    })).rejects.toThrow('WORK_VM_VGPU_LICENSE_STATUS_MISSING')
    expect(calls).toHaveLength(1)

    calls.length = 0
    await expect(readRealWorkVmLicenseStatus({}, {}, {
      deadlineMs: 100,
      runSsh: async (...args) => {
        calls.push(args)
        throw new Error('WORK_SSH_COMMAND_TIMEOUT_GPU_LICENSE')
      },
    })).rejects.toThrow('WORK_SSH_COMMAND_TIMEOUT_GPU_LICENSE')
    expect(calls).toHaveLength(1)
  })

  it('resolves VM workspace files beneath the authorized SSH account home', () => {
    expect(realWorkVmWorkspaceRelativePath('workspace/persistence-marker.txt'))
      .toBe('workspace/persistence-marker.txt')
    expect(() => realWorkVmWorkspaceRelativePath('/workspace/persistence-marker.txt'))
      .toThrow('WORK_VM_WORKSPACE_PATH_INVALID')
    expect(() => realWorkVmWorkspaceRelativePath('workspace/../etc/passwd'))
      .toThrow('WORK_VM_WORKSPACE_PATH_INVALID')
    expect(() => realWorkVmWorkspaceRelativePath('workspace//persistence-marker.txt'))
      .toThrow('WORK_VM_WORKSPACE_PATH_INVALID')
  })

  it('terminates a process at its configured deadline', async () => {
    const result = await runProcess(
      process.execPath,
      ['-e', "require('node:net').createServer().listen(0)"],
      { timeoutMs: 100, outputCode: 'PROCESS_TEST', outputLimitBytes: 1024 },
    )

    expect(result).toMatchObject({ code: null, timedOut: true, outputExceeded: false })
  })

  it('marks an explicit deadline kill as a timeout on every platform', async () => {
    const result = await runProcess(
      process.execPath,
      ['-e', 'setInterval(() => {}, 1000)'],
      { timeoutMs: 100, outputCode: 'PROCESS_TEST', outputLimitBytes: 1024 },
    )

    expect(result.timedOut).toBe(true)
    expect(result.outputExceeded).toBe(false)
  })

  it('does not report an early child SIGKILL as a timeout', async () => {
    const result = await runProcess(
      process.execPath,
      ['-e', "process.kill(process.pid, 'SIGKILL')"],
      { timeoutMs: 5000, outputCode: 'PROCESS_TEST', outputLimitBytes: 1024 },
    )

    expect(result).toMatchObject({ timedOut: false, outputExceeded: false })
    expect(result.signal === 'SIGKILL'
      || (process.platform === 'win32' && Number.isInteger(result.code) && result.code !== 0)).toBe(true)
  })

  it('terminates a process and reports output beyond the configured cap', async () => {
    const result = await runProcess(
      process.execPath,
      ['-e', "process.stdout.write('too-much-output')"],
      { timeoutMs: 5000, outputCode: 'PROCESS_TEST', outputLimitBytes: 4 },
    )

    expect(result).toMatchObject({ timedOut: false, outputExceeded: true })
    expect(Buffer.byteLength(result.stdout) + Buffer.byteLength(result.stderr)).toBeLessThanOrEqual(4)
  })

  it('keeps process start failures on their established diagnostic path', async () => {
    await expect(runProcess(
      'labweaver-program-that-does-not-exist',
      [],
      { timeoutMs: 1000, outputCode: 'PROCESS_TEST', outputLimitBytes: 4 },
    )).rejects.toThrow('PROCESS_TEST_START_FAILED')
  })

  it('distinguishes a remote SSH termination from a local process signal', () => {
    expect(classifyPinnedSshSessionClose({
      code: 255,
      signal: null,
      stderr: 'Connection to gateway.example closed by remote host.',
    })).toBe('remote_terminated')
    expect(classifyPinnedSshSessionClose({ code: null, signal: 'SIGKILL', stderr: '' }))
      .toBe('process_signaled')
  })

  it('classifies explicit OpenSSH failures without exposing stderr details', () => {
    expect(classifyPinnedSshCommandFailure({
      code: 255,
      signal: null,
      stderr: 'Permission denied (publickey).\nPlease try again.',
    })).toBe('WORK_SSH_COMMAND_PERMISSION_DENIED_PUBLICKEY_EXIT_255')
    expect(classifyPinnedSshCommandFailure({
      code: 255,
      signal: null,
      stderr: 'Host key verification failed. /home/private/id_ed25519',
    })).toBe('WORK_SSH_COMMAND_HOST_KEY_VERIFICATION_FAILED_EXIT_255')
    expect(classifyPinnedSshCommandFailure({
      code: 255,
      signal: null,
      stderr: 'ssh: connect to host 10.0.0.8 port 2222: Connection refused',
    })).toBe('WORK_SSH_COMMAND_CONNECTION_REFUSED_EXIT_255')
    expect(classifyPinnedSshCommandFailure({
      code: 255,
      signal: null,
      stderr: 'ssh: connect to host 10.0.0.8 port 2222: Connection timed out',
    })).toBe('WORK_SSH_COMMAND_CONNECTION_TIMEOUT_EXIT_255')
  })

  it('keeps only the gateway diagnostic token and safe stage with the exit status', () => {
    const classified = classifyPinnedSshCommandFailure({
      code: 255,
      signal: null,
      stderr: '{"diagnostic_code":"LW_GATEWAY_TARGET_SESSION_FAILED","failure_stage":"gateway.target_session","safe_detail":"redacted"}',
    })
    expect(classified).toBe('WORK_SSH_COMMAND_LW_GATEWAY_TARGET_SESSION_FAILED_STAGE_GATEWAY_TARGET_SESSION_EXIT_255')
    expect(classified).not.toContain('redacted')
    expect(classified).not.toContain('10.0.0.8')
    expect(classifyPinnedSshCommandFailure({
      code: 255,
      signal: null,
      stderr: '{"diagnostic_code":"LW_GATEWAY_TARGET_SESSION_FAILED","failure_stage":"user-content"}',
    })).toBe('WORK_SSH_COMMAND_LW_GATEWAY_TARGET_SESSION_FAILED_EXIT_255')
  })

  it('keeps a specific OpenSSH failure when gateway diagnostics are also present', () => {
    const classified = classifyPinnedSshCommandFailure({
      code: 255,
      signal: null,
      stderr: 'Permission denied (publickey).\n{"diagnostic_code":"LW_GATEWAY_TARGET_SESSION_FAILED","failure_stage":"gateway.target_session","username":"redacted"}',
    })
    expect(classified).toBe('WORK_SSH_COMMAND_PERMISSION_DENIED_PUBLICKEY_GATEWAY_LW_GATEWAY_TARGET_SESSION_FAILED_STAGE_GATEWAY_TARGET_SESSION_EXIT_255')
    expect(classified).not.toContain('username')
    expect(classified).not.toContain('redacted')
  })

  it('retains signal status when a process has no numeric exit code', () => {
    expect(classifyPinnedSshCommandFailure({
      code: null,
      signal: 'SIGKILL',
      stderr: '',
    })).toBe('WORK_SSH_COMMAND_FAILED_SIGNAL_SIGKILL')
  })

  it('preserves known guest diagnostics before gateway failure tokens', () => {
    const stderr = `Traceback (most recent call last):
AssertionError: CONFIG_PROBE_SSH_INACTIVE
{"diagnostic_code":"LW_GATEWAY_TARGET_SESSION_FAILED","failure_stage":"gateway.target_session"}`
    expect(classifyPinnedSshGuestFailure(stderr)).toBe('CONFIG_PROBE_SSH_INACTIVE')
    expect(classifyPinnedSshGuestFailure(
      "ModuleNotFoundError: No module named 'apt' (/guest/private/config-probe.py)\nLW_GATEWAY_TARGET_SESSION_FAILED",
    )).toBe('WORK_SSH_GUEST_MODULE_MISSING_APT')
    expect(classifyPinnedSshGuestFailure("ModuleNotFoundError: No module named 'yaml'"))
      .toBeNull()
  })
})


function runCanonicalShell(lines, errexit = false) {
  const output = execFileSync('python3', ['-c', String.raw`
import json,os,pty,re,select,signal,sys,termios,time
request=json.load(sys.stdin)
lines=request["lines"]
pid,fd=pty.fork()
if pid==0:
    os.execl('/bin/sh','sh','-i',*(['-e'] if request['errexit'] else []))
captured=b''
def drain(wait):
    global captured
    end=time.monotonic()+wait
    while time.monotonic()<end:
        readable,_,_=select.select([fd],[],[],max(0,end-time.monotonic()))
        if not readable: break
        try: captured+=os.read(fd,65536)
        except OSError: break
try:
    canonical=bool(termios.tcgetattr(fd)[3]&termios.ICANON)
    drain(.1)
    for line in lines:
        os.write(fd,(line+'\r').encode())
        drain(.02)
    deadline=time.monotonic()+5
    while re.search(rb'(?:^|\n)TEST_DONE:\d+\r?\n',captured) is None and time.monotonic()<deadline:
        drain(.1)
    text=captured.replace(b'\r',b'').decode('utf8',errors='replace')
    # A canonical PTY may echo the here-document terminator with only a
    # carriage return. After normalization that leaves the first diagnostic
    # token adjacent to the echo, while subsequent lines still start cleanly.
    # Match the unique diagnostic tokens themselves, while retaining the
    # complete hash and byte-count assertions below.
    digest=re.search(r'PAYLOAD_SHA256=([a-f0-9]{64})(?:\n|$)',text)
    size=re.search(r'PAYLOAD_BYTES=(\d+)(?:\n|$)',text)
    exit_code=re.findall(r'(?:^|\n)TEST_DONE:(\d+)\n',text)
    print(json.dumps({'canonical':canonical,'sha256':digest.group(1) if digest else None,'bytes':int(size.group(1)) if size else None,'exit':exit_code,'shellAlive':bool(re.search(r'(?:^|\n)SHELL_ALIVE\n',text))}))
finally:
    try: os.write(fd,b'exit\r'); drain(.1)
    except OSError: pass
    os.close(fd)
    end=time.monotonic()+1
    while time.monotonic()<end:
        found,_=os.waitpid(pid,os.WNOHANG)
        if found: break
        time.sleep(.01)
    else:
        os.kill(pid,signal.SIGKILL); os.waitpid(pid,0)
`], { input: JSON.stringify({ lines, errexit }), encoding: 'utf8', timeout: 10_000 })
  return JSON.parse(output)
}

describe('real GPU terminal transport', () => {
  it('transports the unchanged CUDA script in short, quoted here-document lines', () => {
    const command = cudaProbeTerminalCommand()
    const lines = command.split('\n')
    expect(lines[0]).toContain("<<'LW_CUDA_SCRIPT'")
    expect(lines.at(-1)).toBe('LW_CUDA_SCRIPT')
    expect(lines.slice(1, -1).every((line) => Buffer.byteLength(line) <= 512)).toBe(true)
    expect(Buffer.from(lines.slice(1, -1).join(''), 'base64')).toEqual(Buffer.from(CUDA_DRIVER_PROBE))
    expect(terminalCommandLines(command, 'TEST_DONE').every((line) => Buffer.byteLength(line) <= 1024)).toBe(true)
  })

  it('requires a complete unique result line rather than command echo, prompts, or partial frames', () => {
    expect(terminalCommandExit("printf '\\nTEST_DONE:%s\\n' 0\r\n> TEST_DONE:0\r\n", 'TEST_DONE')).toBeNull()
    expect(terminalCommandExit('\nTEST_DONE:0', 'TEST_DONE')).toBeNull()
    expect(terminalCommandExit('\nTEST_DONE:7\r\n', 'TEST_DONE')).toBe(7)
    expect(terminalCommandExit('\nTEST_DONE:0\r\n', 'TEST_DONE')).toBe(0)
    expect(() => terminalCommandExit('\nTEST_DONE:0\nTEST_DONE:1\n', 'TEST_DONE')).toThrow('TERMINAL_COMMAND_MARKER_INVALID')
    expect(() => terminalCommandExit('\nTEST_DONE:999\n', 'TEST_DONE')).toThrow('TERMINAL_COMMAND_MARKER_INVALID')
  })

  it('normalizes terminal ANSI sequences and exposes only known CUDA diagnostics on failure', () => {
    const escape = String.fromCharCode(27)
    const output = `${escape}[31mCUDA_DRIVER_CALL_FAILED:cuInit:100${escape}[0m\r\n${escape}]0;private title${String.fromCharCode(7)}`
    expect(normalizeTerminalOutput(output)).toBe('CUDA_DRIVER_CALL_FAILED:cuInit:100\n')
    expect(terminalCommandFailure(output, 71).message)
      .toBe('TERMINAL_COMMAND_FAILED:exit=71:cuda=CUDA_DRIVER_CALL_FAILED:cuInit:100')
    expect(terminalCommandFailure('CUDA_DRIVER_SECRET=private-content\nother private content', null).message)
      .toBe('TERMINAL_COMMAND_MARKER_MISSING:exit=null:cuda=none')
    expect(() => terminalCommandLines('x'.repeat(4096), 'TEST_DONE')).toThrow('TERMINAL_COMMAND_INPUT_LINE_INVALID')
  })

  it.each(['duplicate', 'out-of-range'])('rejects %s markers immediately through the terminal frame and actual poll boundary', async (kind) => {
    const frames = []
    const locator = {
      _apiName: 'Locator',
      _expect: async () => ({ matches: true }),
      click: async () => {},
      focus: async () => {},
    }
    let line = ''
    const page = {
      getByRole: () => locator,
      locator: () => locator,
      keyboard: {
        type: async (value) => { line = value },
        press: async () => {
          const marker = line.match(/(TEST_DONE-[0-9a-f-]+):%s/)?.[1]
          if (marker) frames.push(kind === 'duplicate' ? `\n${marker}:0\n${marker}:1\n` : `\n${marker}:999\n`)
        },
      },
    }
    await expect(typeTerminalCommand(page, locator, frames, 'true', 'TEST_DONE'))
      .rejects.toThrow('TERMINAL_COMMAND_MARKER_INVALID')
  }, 2000)

  it.runIf(process.platform === 'linux')('captures nonzero exits and keeps normal and errexit shells alive', () => {
    const lines = [...terminalCommandLines('false', 'TEST_DONE'), "printf 'SHELL_ALIVE\\n'"]
    for (const errexit of [false, true]) {
      expect(runCanonicalShell(lines, errexit)).toMatchObject({
        canonical: true,
        exit: ['1'],
        shellAlive: true,
      })
    }
  })

  it.runIf(process.platform === 'linux')('roundtrips the full probe through a real canonical /bin/sh PTY without executing CUDA', () => {
    const command = cudaProbeTerminalCommand().split('\n')
    // The receiver replaces only execution at the external interpreter boundary with a hash.
    command[0] = "python3 -c 'import base64,hashlib,sys; data=base64.b64decode(b\"\".join(sys.stdin.buffer.read().split()), validate=True); print(\"PAYLOAD_SHA256=\"+hashlib.sha256(data).hexdigest()); print(\"PAYLOAD_BYTES=\"+str(len(data)))' <<'LW_CUDA_SCRIPT'"
    const lines = terminalCommandLines(command.join('\n'), 'TEST_DONE')
    const output = runCanonicalShell(lines)
    expect(output).toEqual({
      canonical: true,
      sha256: createHash('sha256').update(CUDA_DRIVER_PROBE).digest('hex'),
      bytes: Buffer.byteLength(CUDA_DRIVER_PROBE),
      exit: ['0'],
      shellAlive: false,
    })
  })
})
