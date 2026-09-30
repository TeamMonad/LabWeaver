import { describe, expect, it } from 'vitest'
import {
  openSshPublicKeyFingerprint,
  parseRealWorkVmLicenseStatus,
  runProcess,
  VM_CUDA_PROBE_PTX_TARGET,
} from '../e2e/support/real-work-ssh.mjs'

describe('real Work SSH helper', () => {
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

  it('terminates a process at its configured deadline', async () => {
    const result = await runProcess(
      process.execPath,
      ['-e', "require('node:net').createServer().listen(0)"],
      { timeoutMs: 100, outputCode: 'PROCESS_TEST', outputLimitBytes: 1024 },
    )

    expect(result).toMatchObject({ code: null, timedOut: true, outputExceeded: false })
  })

  it('does not report an early child SIGKILL as a timeout', async () => {
    const result = await runProcess(
      process.execPath,
      ['-e', "process.kill(process.pid, 'SIGKILL')"],
      { timeoutMs: 5000, outputCode: 'PROCESS_TEST', outputLimitBytes: 1024 },
    )

    expect(result).toMatchObject({ code: null, timedOut: false, outputExceeded: false })
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
})
