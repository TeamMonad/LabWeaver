import { expect } from '@playwright/test'
import { pollJson, uuidv7 } from './live.mjs'

export const CUDA_PROBE_PTX_TARGET = 'sm_60'

export const CUDA_DRIVER_PROBE = String.raw`import ctypes
import sys

PTX = b"""\
.version 6.0
.target ${CUDA_PROBE_PTX_TARGET}
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


export function parseCudaProbeResult(output) {
  const resultLine = output
    .split(/\r?\n/)
    .map((line) => line.trim())
    .find((line) => /^LABWEAVER_CUDA_RESULT\s/.test(line))
  const match = resultLine?.match(/^LABWEAVER_CUDA_RESULT count=(\d+) sum=(\d+) max=(\d+)$/)
  if (!match) throw new Error('CUDA_PROBE_RESULT_MISSING')
  const result = { count: Number(match[1]), sum: Number(match[2]), max: Number(match[3]) }
  if (result.count !== 256 || result.sum !== 32640 || result.max !== 255) {
    throw new Error('CUDA_PROBE_RESULT_MISMATCH')
  }
  return result
}

export async function issueAccessGrantAndConnect(page, projectId, environmentId) {
  await page.goto(`/student/environments?projectId=${encodeURIComponent(projectId)}&environmentId=${encodeURIComponent(environmentId)}`, {
    waitUntil: 'domcontentloaded',
  })
  const environmentIdDetails = page.locator('details.environment-id-details')
  await expect(environmentIdDetails).toBeVisible({ timeout: 120_000 })
  await environmentIdDetails.locator('summary').click()
  await expect(environmentIdDetails.locator('code')).toHaveText(environmentId, { timeout: 30_000 })
  const grantButton = page.getByRole('button', { name: '签发访问授权', exact: true })
  if (await grantButton.count() > 0) {
    await expect(grantButton).toBeEnabled({ timeout: 120_000 })
    await grantButton.click()
  }
  await pollJson(
    page.request,
    `/api/v1/environments/${environmentId}/access-grants?includeTerminal=false&limit=10`,
    (value) => Array.isArray(value.items) && value.items.some((item) => item.state === 'active'),
    'LAB_EXPERIMENT_ACCESS_GRANT_ACTIVE_TIMEOUT',
    120_000,
  )
  await page.getByRole('button', { name: 'Web 控制台', exact: true }).click()
  const reconnect = page.getByRole('button', { name: /重新连接终端|重新签发授权并连接终端|立即签发授权并连接终端/ })
  if (await reconnect.count() > 0) {
    await expect(reconnect).toBeEnabled({ timeout: 120_000 })
    await reconnect.click()
  }
  const consolePanel = page.locator('.console-panel')
  await expect(consolePanel).toBeVisible({ timeout: 120_000 })
  const openTerminal = consolePanel.getByRole('button', { name: '打开终端', exact: true })
  if (await openTerminal.count() > 0) {
    await expect(openTerminal).toBeEnabled({ timeout: 120_000 })
    await openTerminal.click()
  }
  const host = page.locator('.xterm-host')
  await expect(host).toBeVisible({ timeout: 120_000 })
  const input = page.locator('.xterm-helper-textarea')
  await expect(input).toBeAttached({ timeout: 30_000 })
  return { input }
}

export function normalizeTerminalOutput(value) {
  const escape = String.fromCharCode(27)
  const bell = String.fromCharCode(7)
  const ansiPattern = new RegExp(`${escape}(?:\\[[0-?]*[ -/]*[@-~]|\\][^${bell}]*(?:${bell}|${escape}\\\\))`, 'g')
  return value
    .replace(ansiPattern, '')
    .replace(/\r/g, '')
}

export function hasTerminalLine(output, expected) {
  const escaped = expected.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  return new RegExp(`(?:^|\\n)${escaped}(?:\\n|$)`).test(normalizeTerminalOutput(output))
}

export async function typeTerminalCommand(page, input, frames, command, marker) {
  const firstFrame = frames.length
  const uniqueMarker = `${marker}-${uuidv7()}`
  await page.getByRole('button', { name: 'Web 控制台', exact: true }).click()
  const host = page.locator('.xterm-host')
  await expect(host).toBeVisible({ timeout: 120_000 })
  await host.click()
  await expect(input).toBeAttached({ timeout: 30_000 })
  await input.focus()
  await page.keyboard.type(`{ ${command}; __lw_exit=$?; printf '\\n${uniqueMarker}:%s\\n' "$__lw_exit"; }`)
  await page.keyboard.press('Enter')
  await expect
    .poll(() => {
      const output = normalizeTerminalOutput(frames.slice(firstFrame).join(''))
      const match = output.match(new RegExp(`(?:^|\\n)${uniqueMarker}:(\\d+)(?:\\n|$)`))
      return match?.[1] ?? null
    }, { timeout: 180_000, intervals: [250, 500, 1000] })
    .toBe('0')
  return normalizeTerminalOutput(frames.slice(firstFrame).join(''))
}

/** Run the same real Driver API kernel through the owner's product terminal. */
export async function runTerminalCudaProbe(page, projectId, environmentId) {
  const frames = []
  const capture = (socket) => {
    socket.on('framereceived', ({ payload }) => {
      frames.push(typeof payload === 'string' ? payload : Buffer.from(payload).toString('utf8'))
    })
  }
  page.on('websocket', capture)
  try {
    const terminal = await issueAccessGrantAndConnect(page, projectId, environmentId)
    const encoded = Buffer.from(CUDA_DRIVER_PROBE).toString('base64')
    const command = `python3 -c 'import base64; exec(base64.b64decode("${encoded}"))'`
    const output = await typeTerminalCommand(page, terminal.input, frames, command, 'LABWEAVER_CAPACITY_CUDA_DONE')
    return parseCudaProbeResult(output)
  } finally {
    page.off('websocket', capture)
  }
}
