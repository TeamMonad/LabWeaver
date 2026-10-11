import { expect, test } from '@playwright/test'
import { stat } from 'node:fs/promises'
import { expectJson, navigateFromHomeByUi, pollJson } from '../support/live.mjs'
import {
  assertAcceptedVgpuImageCompletion,
  matchesVgpuImageCatalogRow,
  validateVgpuImageImport,
} from '../support/vgpu-image-import.mjs'

const INPUT_ENV = Object.freeze({
  archivePath: 'LABWEAVER_E2E_VGPU_IMAGE_ARCHIVE',
  binding: 'LABWEAVER_E2E_VGPU_IMAGE_BINDING',
  targetReference: 'LABWEAVER_E2E_VGPU_IMAGE_TARGET_REFERENCE',
  capacityBytes: 'LABWEAVER_E2E_VGPU_IMAGE_CAPACITY_BYTES',
  diskPath: 'LABWEAVER_E2E_VGPU_IMAGE_DISK_PATH',
})

const REQUIRED_INPUT_FIELDS = Object.keys(INPUT_ENV)
const TRUST_REVISION = 1
const DISK_FORMAT = 'qcow2'
const UPLOAD_EXPIRY_GRACE_MS = 2 * 60 * 1000
const IMPORT_PROCESSING_TIMEOUT_MS = 15 * 60 * 1000
const IMAGE_IMPORT_TEST_TIMEOUT_MS = 60 * 60 * 1000 + IMPORT_PROCESSING_TIMEOUT_MS + UPLOAD_EXPIRY_GRACE_MS

function inputError(detail) {
  throw new Error(`LW_VGPU_IMAGE_INPUT_INVALID:${detail}`)
}

function configuredInput() {
  const input = Object.fromEntries(
    Object.entries(INPUT_ENV).map(([field, variable]) => [field, process.env[variable]?.trim() || '']),
  )
  const presentFields = Object.values(input).filter((value) => value !== '')
  if (presentFields.length === 0) return null
  if (presentFields.length !== REQUIRED_INPUT_FIELDS.length) inputError('required-fields-incomplete')

  if (!/^\d+$/.test(input.capacityBytes)) inputError('capacity-bytes-must-be-positive-integer')
  const capacityBytes = Number(input.capacityBytes)
  if (!Number.isSafeInteger(capacityBytes) || capacityBytes < 1) inputError('capacity-bytes-out-of-range')
  if (!input.binding || input.binding.length > 128) inputError('binding-invalid')
  if (!input.targetReference || input.targetReference.length > 512) inputError('target-reference-invalid')
  if (!input.diskPath || input.diskPath.length > 256 || input.diskPath.startsWith('/') || input.diskPath.endsWith('/')) {
    inputError('disk-path-invalid')
  }
  if (input.diskPath.includes('..') || input.diskPath.split('/').some((segment) => segment.length === 0)) {
    inputError('disk-path-invalid')
  }

  return Object.freeze({ ...input, capacityBytes })
}

const INPUT = configuredInput()
const RUN_LIFECYCLE = process.env.LABWEAVER_E2E_VGPU_IMAGE_LIFECYCLE === '1'

function derivedInput(input, suffix) {
  const binding = `${input.binding}-${suffix}`
  if (binding.length > 128) inputError(`${suffix}-binding-invalid`)
  return Object.freeze({ ...input, binding })
}

async function assertArchiveReadable(input) {
  let archiveMetadata
  try {
    archiveMetadata = await stat(input.archivePath)
  } catch {
    throw new Error('LW_VGPU_IMAGE_ARCHIVE_NOT_READABLE')
  }
  if (!archiveMetadata.isFile() || archiveMetadata.size < 1) {
    throw new Error('LW_VGPU_IMAGE_ARCHIVE_NOT_READABLE')
  }
}

async function fillVmUploadForm(page, input, reason) {
  const uploadCard = page.locator('section.upload-card')
  await uploadCard.getByLabel('类型', { exact: true }).selectOption('virtual_machine')
  await uploadCard.getByLabel('binding', { exact: true }).fill(input.binding)
  await uploadCard.getByLabel('目标引用（host/repo:tag）', { exact: true }).fill(input.targetReference)
  await uploadCard.getByLabel('信任版本', { exact: true }).fill(String(TRUST_REVISION))
  await uploadCard.getByLabel('磁盘格式', { exact: true }).selectOption(DISK_FORMAT)
  await uploadCard.getByLabel('容量（字节）', { exact: true }).fill(String(input.capacityBytes))
  await uploadCard.getByLabel('归档内磁盘路径', { exact: true }).fill(input.diskPath)
  await uploadCard.getByLabel('原因', { exact: true }).fill(reason)
  await uploadCard.locator('input[type="file"]').setInputFiles(input.archivePath)
  return uploadCard
}

function waitForResponseSafely(page, predicate, options) {
  const responsePromise = page.waitForResponse(predicate, options)
  void responsePromise.catch(() => undefined)
  return responsePromise
}

function waitForCompletionResponse(page, predicate, timeout) {
  let settled = false
  let resolveResponse
  let rejectResponse
  const promise = new Promise((resolve, reject) => {
    resolveResponse = resolve
    rejectResponse = reject
  })
  const cleanup = () => page.off('response', onResponse)
  const settle = (error, response) => {
    if (settled) return
    settled = true
    cleanup()
    if (error) rejectResponse(error)
    else resolveResponse(response)
  }
  const onResponse = (response) => {
    if (settled) return
    try {
      if (!predicate(response)) return
    } catch (error) {
      settle(error)
      return
    }
    settle(null, response)
  }
  void promise.catch(() => undefined)
  page.on('response', onResponse)
  return {
    promise,
    timeout,
    cancel() {
      settle(new Error('LW_VGPU_IMAGE_COMPLETION_RESPONSE_CANCELLED'))
    },
  }
}

async function readUploadUiDiagnostic(page) {
  const status = page.locator('.upload-status')
  const codeLocator = status.locator('.upload-diagnostic-code')
  if (await codeLocator.count() === 0) return null
  const code = (await codeLocator.first().textContent())?.trim()
  if (!code) return null
  return {
    code,
    message: (await status.locator('p').first().textContent())?.trim() || '',
  }
}

async function waitForUploadCompletionOrUiFailure(page, completionResponseWaiter, previousDiagnostic = null) {
  let responseSettled = false
  let pollCancelled = false
  let latestDiagnostic = null
  const completionPromise = completionResponseWaiter.promise.then(
    (response) => {
      responseSettled = true
      return { kind: 'completion', response }
    },
    (error) => {
      responseSettled = true
      throw error
    },
  )
  void completionPromise.catch(() => undefined)

  const diagnosticOrResponse = expect.poll(async () => {
    if (pollCancelled) return 'cancelled'
    if (responseSettled) return 'response'
    const diagnostic = await readUploadUiDiagnostic(page)
    if (responseSettled) return 'response'
    if (
      previousDiagnostic
      && diagnostic
      && diagnostic.code === previousDiagnostic.code
      && diagnostic.message === previousDiagnostic.message
    ) return 'pending'
    if (diagnostic) {
      latestDiagnostic = diagnostic
      return 'diagnostic'
    }
    return 'pending'
  }, {
    timeout: completionResponseWaiter.timeout,
    intervals: [500, 1000, 2000],
  }).toMatch(/^(response|diagnostic|cancelled)$/).then(async () => {
    if (pollCancelled) return { kind: 'cancelled' }
    if (responseSettled) return completionPromise
    if (!latestDiagnostic) throw new Error('LW_VGPU_IMAGE_UPLOAD_UI_DIAGNOSTIC_MISSING')
    return { kind: 'diagnostic', failure: latestDiagnostic }
  })
  void diagnosticOrResponse.catch(() => undefined)

  try {
    const outcome = await Promise.race([completionPromise, diagnosticOrResponse])
    if (outcome.kind === 'diagnostic') {
      const error = new Error(`LW_VGPU_IMAGE_UPLOAD_UI_FAILED:${outcome.failure.code}:${outcome.failure.message}`)
      error.code = outcome.failure.code
      throw error
    }
    return outcome.response
  } finally {
    pollCancelled = true
    completionResponseWaiter.cancel()
  }
}

async function waitForFirstCompletionFaultOrUiFailure(
  page,
  faultPromise,
  timeout,
  isFaultInjectionInProgress,
  previousDiagnostic = null,
) {
  let faultSettled = false
  let pollCancelled = false
  let latestDiagnostic = null
  const faultOutcome = faultPromise.then(
    (url) => {
      faultSettled = true
      return { kind: 'fault', url }
    },
    (error) => {
      faultSettled = true
      throw error
    },
  )
  void faultOutcome.catch(() => undefined)

  const diagnosticOrFault = expect.poll(async () => {
    if (pollCancelled) return 'cancelled'
    if (faultSettled) return 'fault'
    if (isFaultInjectionInProgress()) return 'pending'
    const diagnostic = await readUploadUiDiagnostic(page)
    if (faultSettled) return 'fault'
    if (isFaultInjectionInProgress()) return 'pending'
    if (
      previousDiagnostic
      && diagnostic
      && diagnostic.code === previousDiagnostic.code
      && diagnostic.message === previousDiagnostic.message
    ) return 'pending'
    if (diagnostic) {
      latestDiagnostic = diagnostic
      return 'diagnostic'
    }
    return 'pending'
  }, {
    timeout,
    intervals: [500, 1000, 2000],
  }).toMatch(/^(fault|diagnostic|cancelled)$/).then(() => {
    if (pollCancelled) return { kind: 'cancelled' }
    if (faultSettled) return faultOutcome
    if (!latestDiagnostic) throw new Error('LW_VGPU_IMAGE_UPLOAD_UI_DIAGNOSTIC_MISSING')
    return { kind: 'diagnostic', failure: latestDiagnostic }
  })
  void diagnosticOrFault.catch(() => undefined)

  try {
    const outcome = await Promise.race([faultOutcome, diagnosticOrFault])
    if (outcome.kind === 'diagnostic') {
      const error = new Error(`LW_VGPU_IMAGE_UPLOAD_UI_FAILED:${outcome.failure.code}:${outcome.failure.message}`)
      error.code = outcome.failure.code
      throw error
    }
    return outcome.url
  } finally {
    pollCancelled = true
  }
}

async function waitForUploadSession(page, input) {
  const uploadCard = await fillVmUploadForm(page, input, '导入已评审的 vGPU guest image。')
  const importButton = uploadCard.getByRole('button', { name: '上传并导入', exact: true })
  await expect(importButton).toBeEnabled()
  const sessionResponsePromise = waitForResponseSafely(page, (response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === '/api/v1/admin/images/uploads'
  })
  await importButton.click()
  const sessionResponse = await sessionResponsePromise
  const session = await expectJson(sessionResponse, 'LW_VGPU_IMAGE_UPLOAD_SESSION_READ_FAILED')
  if (typeof session.uploadId !== 'string' || session.uploadId.length === 0) {
    throw new Error('LW_VGPU_IMAGE_UPLOAD_SESSION_INVALID')
  }
  if (session.uploadTarget?.partSizeBytes !== 64 * 1024 * 1024
    || !Array.isArray(session.uploadTarget.parts)
    || session.uploadTarget.parts.length === 0
    || !Array.isArray(session.uploadedParts)
    || session.uploadTarget.parts.some((part, index) => (
      part?.partNumber !== index + 1
      || typeof part.uploadUrl !== 'string'
      || typeof part.expiresAt !== 'string'
    ))) {
    throw new Error('LW_VGPU_IMAGE_MULTIPART_TARGET_INVALID')
  }
  if (typeof session.uploadTarget?.expiresAt !== 'string' || !Number.isFinite(Date.parse(session.uploadTarget.expiresAt))) {
    throw new Error('LW_VGPU_IMAGE_UPLOAD_EXPIRY_INVALID')
  }
  return { uploadCard, importButton, session }
}

async function readUploadStatus(page, uploadId, failureCode = 'LW_VGPU_IMAGE_UPLOAD_STATUS_READ_FAILED') {
  return expectJson(
    await page.request.get(`/api/v1/admin/images/uploads/${encodeURIComponent(uploadId)}`),
    failureCode,
  )
}

async function waitForUploadedPart(page, uploadId, timeout) {
  let latestStatus
  await expect.poll(async () => {
    latestStatus = await readUploadStatus(page, uploadId)
    if (latestStatus.state !== 'pending') {
      throw new Error(`LW_VGPU_IMAGE_UPLOAD_FINISHED_BEFORE_PART:${String(latestStatus.state)}`)
    }
    return Array.isArray(latestStatus.uploadedParts) && latestStatus.uploadedParts.length > 0
  }, { timeout, intervals: [500, 1000, 2000] }).toBe(true)
  return latestStatus
}

function uploadCompletionTimeout(session) {
  const expiresAt = Date.parse(session.uploadTarget?.expiresAt ?? '')
  if (!Number.isFinite(expiresAt) || expiresAt <= Date.now()) {
    throw new Error('LW_VGPU_IMAGE_UPLOAD_EXPIRY_INVALID')
  }
  return expiresAt - Date.now() + UPLOAD_EXPIRY_GRACE_MS + IMPORT_PROCESSING_TIMEOUT_MS
}

async function waitForUploadStatus(page, uploadId, state) {
  await expect.poll(async () => {
    const status = await readUploadStatus(page, uploadId)
    return status.state === state
  }, { timeout: 120_000, intervals: [500, 1000, 2000] }).toBe(true)
}

function readDiagnosticCode(alert) {
  return alert.locator('.diagnostic-code').textContent().then((value) => value?.trim() || 'diagnostic-missing')
}

async function waitForCatalogSettled(page, operation) {
  await expect.poll(async () => {
    const alert = page.locator('[role="alert"]')
    if (await alert.count()) {
      throw new Error(`LW_VGPU_IMAGE_${operation}_FAILED:${await readDiagnosticCode(alert.first())}`)
    }
    if (await page.locator('[role="region"][aria-label="平台镜像目录"]').count()) return 'ready'
    if (await page.getByText('平台镜像目录为空。', { exact: true }).count()) return 'empty'
    return 'loading'
  }, { timeout: 60_000, intervals: [250, 500, 1000] }).toMatch(/^(ready|empty)$/)
}

async function readRowsForBinding(page, binding) {
  const region = page.locator('[role="region"][aria-label="平台镜像目录"]')
  if (await region.count() === 0) return []
  return region.locator('tbody tr.data-table__row').evaluateAll((rowElements, targetBinding) => rowElements
    .map((row) => [...row.querySelectorAll('td')].map((cell) => cell.textContent?.trim() || ''))
    .filter((cells) => cells[1] === targetBinding)
    .map((cells) => ({
      kindLabel: cells[0],
      binding: cells[1],
      sourceReference: cells[2],
      capacityLabel: cells[6],
      format: cells[8],
      statusLabel: cells[9],
      trustRevision: Number(cells[10]),
    })), binding)
}

async function waitForImportReadback(page, input) {
  await expect.poll(async () => {
    const alert = page.locator('[role="alert"]')
    if (await alert.count()) {
      throw new Error(`LW_VGPU_IMAGE_IMPORT_FAILED:${await readDiagnosticCode(alert.first())}`)
    }
    const rows = await readRowsForBinding(page, input.binding)
    if (rows.length > 1) throw new Error('LW_VGPU_IMAGE_READBACK_DUPLICATE_BINDING')
    return rows.length === 1 && matchesVgpuImageCatalogRow(rows[0], input)
  }, { timeout: IMPORT_PROCESSING_TIMEOUT_MS, intervals: [1000, 2000, 3000] }).toBe(true)
}

async function readCatalogEntries(page) {
  const catalog = await expectJson(
    await page.request.get('/api/v1/admin/images'),
    'LW_VGPU_IMAGE_CATALOG_API_READ_FAILED',
  )
  if (!Array.isArray(catalog?.entries)) throw new Error('LW_VGPU_IMAGE_CATALOG_API_INVALID')
  return catalog.entries
}

async function assertImportedUpload(page, input, completion) {
  const accepted = assertAcceptedVgpuImageCompletion(completion)
  const status = await pollJson(
    page.request,
    `/api/v1/admin/images/uploads/${accepted.uploadId}`,
    (value) => ['imported', 'failed', 'cancelled'].includes(value.state),
    'LW_VGPU_IMAGE_UPLOAD_STATUS_READ_FAILED',
    IMPORT_PROCESSING_TIMEOUT_MS,
  )
  const entries = await readCatalogEntries(page)
  return validateVgpuImageImport({ completion, status, entries, input })
}

async function cleanupCancelledUploadThroughUi(page, input, uploadId) {
  const status = await expectJson(
    await page.request.get(`/api/v1/admin/images/uploads/${encodeURIComponent(uploadId)}`),
    'LW_VGPU_IMAGE_CANCEL_CLEANUP_STATUS_READ_FAILED',
  )
  if (status.uploadId !== uploadId) throw new Error('LW_VGPU_IMAGE_CANCEL_CLEANUP_UPLOAD_ID_MISMATCH')

  if (status.state === 'failed' && status.diagnostic === 'LW_PLATFORM_IMAGE_UPLOAD_EXPIRED') {
    const terminal = await expectJson(
      await page.request.get(`/api/v1/admin/images/uploads/${encodeURIComponent(uploadId)}`),
      'LW_VGPU_IMAGE_EXPIRED_CLEANUP_STATUS_READ_FAILED',
    )
    if (terminal.uploadId !== uploadId || terminal.state !== 'failed' || terminal.diagnostic !== 'LW_PLATFORM_IMAGE_UPLOAD_EXPIRED') {
      throw new Error('LW_VGPU_IMAGE_EXPIRED_CLEANUP_STATUS_CHANGED')
    }
    await waitForCatalogSettled(page, 'EXPIRED_CLEANUP_READBACK')
    if ((await readRowsForBinding(page, input.binding)).length !== 0) {
      throw new Error('LW_VGPU_IMAGE_EXPIRED_CLEANUP_BINDING_PUBLISHED')
    }
    return
  }
  if (status.state === 'imported' || status.state === 'failed') {
    throw new Error(`LW_VGPU_IMAGE_CANCEL_CLEANUP_UNSAFE_STATE:${status.state}`)
  }
  if (!['pending', 'queued', 'freezing', 'importing', 'cancelling', 'cancelled'].includes(status.state)) {
    throw new Error(`LW_VGPU_IMAGE_CANCEL_CLEANUP_UNKNOWN_STATE:${String(status.state)}`)
  }
  if (status.state !== 'cancelled' && status.state !== 'cancelling') {
    const cancelButton = page.locator('.upload-status').getByRole('button', { name: '取消上传', exact: true })
    await expect(cancelButton).toBeVisible({ timeout: 120_000 })
    await expect(cancelButton).toBeEnabled()
    await cancelButton.click()
    await expect(page.locator('.upload-status')).toContainText('镜像导入：已取消', { timeout: 120_000 })
  }

  if (status.state !== 'cancelled') {
    const settled = await pollJson(
      page.request,
      `/api/v1/admin/images/uploads/${encodeURIComponent(uploadId)}`,
      (value) => ['cancelled', 'failed', 'imported'].includes(value.state),
      'LW_VGPU_IMAGE_CANCEL_CLEANUP_STATUS_READ_FAILED',
      120_000,
    )
    if (settled.uploadId !== uploadId) throw new Error('LW_VGPU_IMAGE_CANCEL_CLEANUP_UPLOAD_ID_MISMATCH')
    if (settled.state !== 'cancelled') throw new Error(`LW_VGPU_IMAGE_CANCEL_CLEANUP_UNSAFE_STATE:${settled.state}`)
  }

  await waitForCatalogSettled(page, 'CANCEL_CLEANUP_READBACK')
  if ((await readRowsForBinding(page, input.binding)).length !== 0) {
    throw new Error('LW_VGPU_IMAGE_CANCEL_CLEANUP_BINDING_PUBLISHED')
  }
}

test.skip(INPUT === null, 'set the five LABWEAVER_E2E_VGPU_IMAGE_* variables to run this scenario')

test('platform administrator refreshes a real VM upload and cancels it through the UI', async ({ page }) => {
  test.setTimeout(IMAGE_IMPORT_TEST_TIMEOUT_MS)
  test.skip(!RUN_LIFECYCLE, 'set LABWEAVER_E2E_VGPU_IMAGE_LIFECYCLE=1 for the real upload lifecycle scenarios')
  if (!INPUT) throw new Error('LW_VGPU_IMAGE_INPUT_REQUIRED')
  await assertArchiveReadable(INPUT)

  const input = derivedInput(INPUT, 'cancel')
  await navigateFromHomeByUi(page, '平台镜像')
  await expect(page.getByRole('heading', { name: '平台镜像', exact: true })).toBeVisible()
  await waitForCatalogSettled(page, 'LOAD')
  if ((await readRowsForBinding(page, input.binding)).length > 0) {
    throw new Error('LW_VGPU_IMAGE_CANCEL_BINDING_ALREADY_EXISTS')
  }

  let session
  let primaryError
  let cleanupError
  try {
    ({ session } = await waitForUploadSession(page, input))
    await waitForUploadedPart(page, session.uploadId, uploadCompletionTimeout(session))
    await page.reload({ waitUntil: 'domcontentloaded' })
    await expect(page.getByRole('heading', { name: '平台镜像', exact: true })).toBeVisible()
    await expect(page.locator('.upload-status')).toBeVisible({ timeout: 120_000 })
    const cancelButton = page.locator('.upload-status').getByRole('button', { name: '取消上传', exact: true })
    if (await cancelButton.count() === 0) {
      const status = await expectJson(
        await page.request.get(`/api/v1/admin/images/uploads/${session.uploadId}`),
        'LW_VGPU_IMAGE_CANCEL_STATUS_READ_FAILED',
      )
      throw new Error(`LW_VGPU_IMAGE_IMPORT_FINISHED_BEFORE_CANCEL:${status.state}`)
    }
    await expect(cancelButton).toBeVisible({ timeout: 120_000 })
    await expect(cancelButton).toBeEnabled()
    await cancelButton.click()
    await expect(page.locator('.upload-status')).toContainText('镜像导入：已取消', { timeout: 120_000 })
    await waitForUploadStatus(page, session.uploadId, 'cancelled')
    await waitForCatalogSettled(page, 'CANCEL_READBACK')
    if ((await readRowsForBinding(page, input.binding)).length !== 0) {
      throw new Error('LW_VGPU_IMAGE_CANCELLED_BINDING_PUBLISHED')
    }
  } catch (error) {
    primaryError = error
  } finally {
    if (session?.uploadId) {
      try {
        await cleanupCancelledUploadThroughUi(page, input, session.uploadId)
      } catch (error) {
        cleanupError = error
      }
    }
  }
  if (cleanupError) {
    test.info().annotations.push({
      type: 'image-import-cleanup',
      description: `upload-cancel-cleanup-failed:${cleanupError instanceof Error ? cleanupError.message : String(cleanupError)}`,
    })
  }
  if (primaryError && cleanupError) {
    throw new AggregateError(
      [primaryError, cleanupError],
      'vGPU image import failed and upload cleanup also failed',
      { cause: primaryError },
    )
  }
  if (primaryError) throw primaryError
  if (cleanupError) throw cleanupError
  test.info().annotations.push({
    type: 'image-import-lifecycle',
    description: JSON.stringify({ result: 'refreshed-and-cancelled', binding: input.binding }),
  })
})

test('platform administrator retries a real VM import after a network failure through the UI', async ({ page }) => {
  test.setTimeout(IMAGE_IMPORT_TEST_TIMEOUT_MS)
  test.skip(!RUN_LIFECYCLE, 'set LABWEAVER_E2E_VGPU_IMAGE_LIFECYCLE=1 for the real upload lifecycle scenarios')
  if (!INPUT) throw new Error('LW_VGPU_IMAGE_INPUT_REQUIRED')
  await assertArchiveReadable(INPUT)

  await navigateFromHomeByUi(page, '平台镜像')
  await expect(page.getByRole('heading', { name: '平台镜像', exact: true })).toBeVisible()
  await waitForCatalogSettled(page, 'LOAD')
  const existingRows = await readRowsForBinding(page, INPUT.binding)
  if (existingRows.length > 1) throw new Error('LW_VGPU_IMAGE_READBACK_DUPLICATE_BINDING')
  if (existingRows.length === 1) {
    if (!matchesVgpuImageCatalogRow(existingRows[0], INPUT)) throw new Error('LW_VGPU_IMAGE_BINDING_CONFLICT')
    test.info().annotations.push({
      type: 'image-import-lifecycle',
      description: JSON.stringify({ result: 'reused-existing-after-retry', binding: INPUT.binding }),
    })
    return
  }

  // The first injected failure interrupts one real multipart PUT. The retry
  // below uses the same session and lets the server's uploaded-parts manifest
  // decide which parts still need a PUT before the completion fault is tested.
  let partFailureTargetUrl = null
  let abortOnePart = true
  let partFaultInProgress = false
  let partFaultResolve
  let partFaultReject
  const partFault = new Promise((resolve, reject) => {
    partFaultResolve = resolve
    partFaultReject = reject
  })
  void partFault.catch(() => undefined)
  const partFailureHandler = async (route) => {
    const request = route.request()
    if (
      abortOnePart
      && partFailureTargetUrl
      && request.method() === 'PUT'
      && request.url() === partFailureTargetUrl
    ) {
      abortOnePart = false
      partFaultInProgress = true
      try {
        await route.abort('failed')
        partFaultResolve(request.url())
      } catch (error) {
        partFaultReject(error)
        throw error
      } finally {
        partFaultInProgress = false
      }
      return
    }
    await route.fallback()
  }

  let abortFirstCompletion = true
  let firstCompletionFaultInProgress = false
  let firstCompletionFaultResolve
  let firstCompletionFaultReject
  const firstCompletionFault = new Promise((resolve, reject) => {
    firstCompletionFaultResolve = resolve
    firstCompletionFaultReject = reject
  })
  void firstCompletionFault.catch(() => undefined)
  const completionFailureHandler = async (route) => {
    if (!abortFirstCompletion) return route.continue()
    abortFirstCompletion = false
    firstCompletionFaultInProgress = true
    try {
      await page.context().setOffline(true)
      await route.abort('failed')
      firstCompletionFaultResolve(new URL(route.request().url()))
    } catch (error) {
      firstCompletionFaultReject(error)
      throw error
    } finally {
      firstCompletionFaultInProgress = false
    }
  }
  await page.route('**/*', partFailureHandler)
  await page.route('**/api/v1/admin/images/uploads/*/complete', completionFailureHandler)
  let primaryError
  let cleanupError
  let session
  const observedPartRequests = []
  const onPartRequest = (request) => {
    if (request.method() === 'PUT') observedPartRequests.push(request.url())
  }
  page.on('request', onPartRequest)
  try {
    ({ session } = await waitForUploadSession(page, INPUT))
    if (session.uploadTarget.parts.length < 2) {
      throw new Error('LW_VGPU_IMAGE_MULTIPART_PART_FAULT_TARGET_MISSING')
    }
    const uploadedBeforeFault = await waitForUploadedPart(
      page,
      session.uploadId,
      uploadCompletionTimeout(session),
    )
    if (uploadedBeforeFault.uploadedParts.length === 0) {
      throw new Error('LW_VGPU_IMAGE_MULTIPART_UPLOADED_PART_MISSING')
    }
    const uploadedPartNumbers = new Set(uploadedBeforeFault.uploadedParts.map((part) => part.partNumber))
    const maxUploadedPart = Math.max(...uploadedPartNumbers)
    const unrequestedTargets = session.uploadTarget.parts
      .filter((target) => !observedPartRequests.includes(target.uploadUrl))
      .sort((left, right) => right.partNumber - left.partNumber)
    const subsequentTarget = unrequestedTargets.find((target) => target.partNumber > maxUploadedPart)
      ?? unrequestedTargets[0]
    if (!subsequentTarget) {
      throw new Error('LW_VGPU_IMAGE_MULTIPART_PART_FAULT_TARGET_ALREADY_REQUESTED')
    }
    partFailureTargetUrl = subsequentTarget.uploadUrl
    const partFaultUrl = await waitForFirstCompletionFaultOrUiFailure(
      page,
      partFault,
      uploadCompletionTimeout(session),
      () => partFaultInProgress,
    )
    if (partFaultUrl !== partFailureTargetUrl) {
      throw new Error('LW_VGPU_IMAGE_MULTIPART_PART_FAULT_TARGET_MISMATCH')
    }
    await expect(page.locator('.upload-status')).toBeVisible({ timeout: 120_000 })
    await expect(page.locator('.upload-status')).toContainText('镜像导入：需要操作', { timeout: 120_000 })
    const statusAfterPartFault = await readUploadStatus(page, session.uploadId)
    if (statusAfterPartFault.state !== 'pending' || statusAfterPartFault.uploadedParts.length === 0) {
      throw new Error(`LW_VGPU_IMAGE_MULTIPART_PART_FAULT_STATUS_INVALID:${statusAfterPartFault.state}`)
    }
    const uploadedBeforeRetry = new Set(
      statusAfterPartFault.uploadedParts.map((part) => part.partNumber),
    )
    if (uploadedBeforeRetry.size === 0) throw new Error('LW_VGPU_IMAGE_MULTIPART_RESUME_MANIFEST_EMPTY')

    await page.context().setOffline(false)
    await page.unroute('**/*', partFailureHandler)
    observedPartRequests.length = 0
    const uploadCard = page.locator('section.upload-card')
    await uploadCard.locator('input[type="file"]').setInputFiles(INPUT.archivePath)
    const continueButton = uploadCard.getByRole('button', { name: '继续上传并导入', exact: true })
    await expect(continueButton).toBeVisible({ timeout: 120_000 })
    await expect(continueButton).toBeEnabled()
    const partFailureDiagnostic = await readUploadUiDiagnostic(page)
    await continueButton.click()
    const firstCompletionUrl = await waitForFirstCompletionFaultOrUiFailure(
      page,
      firstCompletionFault,
      uploadCompletionTimeout(session),
      () => firstCompletionFaultInProgress,
      partFailureDiagnostic,
    )
    if (firstCompletionUrl.pathname !== `/api/v1/admin/images/uploads/${session.uploadId}/complete`) {
      throw new Error('LW_VGPU_IMAGE_COMPLETION_UPLOAD_ID_MISMATCH')
    }
    const resumedPartUrls = new Set(observedPartRequests)
    const resumedUploadedPartTargets = statusAfterPartFault.uploadedParts
      .map((part) => session.uploadTarget.parts.find((target) => target.partNumber === part.partNumber)?.uploadUrl)
      .filter((url) => typeof url === 'string')
    if (resumedUploadedPartTargets.some((url) => resumedPartUrls.has(url))) {
      throw new Error('LW_VGPU_IMAGE_MULTIPART_RESUME_REUPLOADED_PART')
    }
    await expect(page.locator('.upload-status')).toBeVisible({ timeout: 120_000 })
    await expect(page.locator('.upload-status')).toContainText('镜像导入：需要操作', { timeout: 120_000 })
    await page.context().setOffline(false)
    await page.unroute('**/api/v1/admin/images/uploads/*/complete', completionFailureHandler)

    const retryButton = page.locator('.upload-status').getByRole('button', { name: '重试导入', exact: true })
    await expect(retryButton).toBeVisible({ timeout: 120_000 })
    await expect(retryButton).toBeEnabled()
    const completionFailureDiagnostic = await readUploadUiDiagnostic(page)
    const completionResponseWaiter = waitForCompletionResponse(page, (response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/admin/images/uploads/${session.uploadId}/complete`
    }, uploadCompletionTimeout(session))
    await retryButton.click()
    const completionResponse = await waitForUploadCompletionOrUiFailure(page, completionResponseWaiter, completionFailureDiagnostic)
    const completion = {
      status: completionResponse.status(),
      body: await completionResponse.json().catch(() => null),
    }
    const imported = await assertImportedUpload(page, INPUT, completion)
    await waitForImportReadback(page, INPUT)
    await expect(page.locator('.upload-status')).toContainText('镜像导入：已导入', { timeout: 60_000 })
    const uiRows = await readRowsForBinding(page, INPUT.binding)
    if (uiRows.length !== 1 || !matchesVgpuImageCatalogRow(uiRows[0], INPUT)) {
      throw new Error(`LW_VGPU_IMAGE_UI_CATALOG_IDENTITY_MISMATCH:${imported.catalogId}`)
    }
    test.info().annotations.push({
      type: 'image-import-lifecycle',
      description: JSON.stringify({ result: 'network-failure-retry', binding: INPUT.binding }),
    })
  } catch (error) {
    primaryError = error
  } finally {
    try {
      await page.context().setOffline(false)
    } catch (error) {
      cleanupError = error
    }
    try {
      await page.unroute('**/*', partFailureHandler)
      await page.unroute('**/api/v1/admin/images/uploads/*/complete', completionFailureHandler)
    } catch (error) {
      cleanupError ??= error
    }
    page.off('request', onPartRequest)
    try {
      if (session?.uploadId) {
        const status = await expectJson(
          await page.request.get(`/api/v1/admin/images/uploads/${encodeURIComponent(session.uploadId)}`),
          'LW_VGPU_IMAGE_RETRY_CLEANUP_STATUS_READ_FAILED',
        )
        if (status.state !== 'imported') {
          await cleanupCancelledUploadThroughUi(page, INPUT, session.uploadId)
        }
      }
    } catch (error) {
      cleanupError ??= error
    }
  }
  if (cleanupError) {
    test.info().annotations.push({
      type: 'image-import-cleanup',
      description: `upload-cancel-cleanup-failed:${cleanupError instanceof Error ? cleanupError.message : String(cleanupError)}`,
    })
  }
  if (primaryError && cleanupError) {
    throw new AggregateError(
      [primaryError, cleanupError],
      'vGPU image import failed and upload cleanup also failed',
      { cause: primaryError },
    )
  }
  if (primaryError) throw primaryError
  if (cleanupError) throw cleanupError
})

test('platform administrator imports one requested vGPU guest image through the UI', async ({ page }) => {
  test.setTimeout(IMAGE_IMPORT_TEST_TIMEOUT_MS)
  if (!INPUT) throw new Error('LW_VGPU_IMAGE_INPUT_REQUIRED')

  await assertArchiveReadable(INPUT)

  // The platform-admin project supplies the existing Keycloak storage state from auth.setup.mjs.
  await navigateFromHomeByUi(page, '平台镜像')
  await expect(page.getByRole('heading', { name: '平台镜像', exact: true })).toBeVisible()
  await waitForCatalogSettled(page, 'LOAD')

  const existingRows = await readRowsForBinding(page, INPUT.binding)
  if (existingRows.length > 1) throw new Error('LW_VGPU_IMAGE_READBACK_DUPLICATE_BINDING')
  if (existingRows.length === 1) {
    if (!matchesVgpuImageCatalogRow(existingRows[0], INPUT)) throw new Error('LW_VGPU_IMAGE_BINDING_CONFLICT')
    test.info().annotations.push({
      type: 'image-import-path',
      description: JSON.stringify({ result: 'reused-existing', binding: INPUT.binding }),
    })
    return
  }

  const { importButton, session } = await waitForUploadSession(page, INPUT)
  const completionResponseWaiter = waitForCompletionResponse(page, (response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && /^\/api\/v1\/admin\/images\/uploads\/[^/]+\/complete$/.test(url.pathname)
  }, uploadCompletionTimeout(session))
  await importButton.click()
  const completionResponse = await waitForUploadCompletionOrUiFailure(page, completionResponseWaiter)
  const completionBody = await completionResponse.json().catch(() => null)
  const completion = { status: completionResponse.status(), body: completionBody }
  const imported = await assertImportedUpload(page, INPUT, completion)
  await waitForImportReadback(page, INPUT)
  await expect(page.locator('.upload-status')).toContainText('镜像导入：已导入', { timeout: 60_000 })
  const uiRows = await readRowsForBinding(page, INPUT.binding)
  if (uiRows.length !== 1 || !matchesVgpuImageCatalogRow(uiRows[0], INPUT)) {
    throw new Error(`LW_VGPU_IMAGE_UI_CATALOG_IDENTITY_MISMATCH:${imported.catalogId}`)
  }
  test.info().annotations.push({
    type: 'image-import-path',
    description: JSON.stringify({ result: 'fresh-upload', uploadId: imported.uploadId, catalogId: imported.catalogId }),
  })
})
