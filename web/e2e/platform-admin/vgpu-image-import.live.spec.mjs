import { expect, test } from '@playwright/test'
import { stat } from 'node:fs/promises'
import { expectJson, pollJson } from '../support/live.mjs'
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
  }, { timeout: 900_000, intervals: [1000, 2000, 3000] }).toBe(true)
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
    900_000,
  )
  const entries = await readCatalogEntries(page)
  return validateVgpuImageImport({ completion, status, entries, input })
}

test.skip(INPUT === null, 'set the five LABWEAVER_E2E_VGPU_IMAGE_* variables to run this scenario')

test('platform administrator imports one requested vGPU guest image through the UI', async ({ page }) => {
  test.setTimeout(1_200_000)
  if (!INPUT) throw new Error('LW_VGPU_IMAGE_INPUT_REQUIRED')

  let archiveMetadata
  try {
    archiveMetadata = await stat(INPUT.archivePath)
  } catch {
    throw new Error('LW_VGPU_IMAGE_ARCHIVE_NOT_READABLE')
  }
  if (!archiveMetadata.isFile() || archiveMetadata.size < 1) {
    throw new Error('LW_VGPU_IMAGE_ARCHIVE_NOT_READABLE')
  }

  // The platform-admin project supplies the existing Keycloak storage state from auth.setup.mjs.
  await page.goto('/admin/platform-images', { waitUntil: 'domcontentloaded' })
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

  const uploadCard = page.locator('section.upload-card')
  await uploadCard.getByLabel('类型', { exact: true }).selectOption('virtual_machine')
  await uploadCard.getByLabel('binding', { exact: true }).fill(INPUT.binding)
  await uploadCard.getByLabel('目标引用（host/repo:tag）', { exact: true }).fill(INPUT.targetReference)
  await uploadCard.getByLabel('信任版本', { exact: true }).fill(String(TRUST_REVISION))
  await uploadCard.getByLabel('磁盘格式', { exact: true }).selectOption(DISK_FORMAT)
  await uploadCard.getByLabel('容量（字节）', { exact: true }).fill(String(INPUT.capacityBytes))
  await uploadCard.getByLabel('归档内磁盘路径', { exact: true }).fill(INPUT.diskPath)
  await uploadCard.getByLabel('原因', { exact: true }).fill('导入已评审的 vGPU guest image。')
  await uploadCard.locator('input[type="file"]').setInputFiles(INPUT.archivePath)

  const importButton = uploadCard.getByRole('button', { name: '上传并导入', exact: true })
  await expect(importButton).toBeEnabled()
  const completionResponsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && /^\/api\/v1\/admin\/images\/uploads\/[^/]+\/complete$/.test(url.pathname)
  }, { timeout: 1_200_000 })
  await importButton.click()
  const completionResponse = await completionResponsePromise
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
