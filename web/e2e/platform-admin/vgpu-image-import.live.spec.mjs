import { expect, test } from '@playwright/test'
import { stat } from 'node:fs/promises'

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
const VM_KIND_LABEL = '虚拟机'
const ACTIVE_STATUS_LABEL = '可用'

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

function formatBytes(bytes) {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  if (bytes === 0) return '0 B'
  const exponent = Math.min(Math.floor(Math.log2(bytes) / 10), units.length - 1)
  const value = bytes / 2 ** (exponent * 10)
  return `${value.toFixed(exponent === 0 ? 0 : 2)} ${units[exponent]}`
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

function rowMatches(row, input) {
  return Boolean(
    row
      && row.kindLabel === VM_KIND_LABEL
      && row.binding === input.binding
      && row.sourceReference === input.targetReference
      && row.capacityLabel === formatBytes(input.capacityBytes)
      && row.format === DISK_FORMAT
      && row.statusLabel === ACTIVE_STATUS_LABEL
      && row.trustRevision === TRUST_REVISION,
  )
}

async function waitForImportReadback(page, input) {
  await expect.poll(async () => {
    const alert = page.locator('[role="alert"]')
    if (await alert.count()) {
      throw new Error(`LW_VGPU_IMAGE_IMPORT_FAILED:${await readDiagnosticCode(alert.first())}`)
    }
    const rows = await readRowsForBinding(page, input.binding)
    if (rows.length > 1) throw new Error('LW_VGPU_IMAGE_READBACK_DUPLICATE_BINDING')
    return rows.length === 1 && rowMatches(rows[0], input)
  }, { timeout: 300_000, intervals: [500, 1000, 2000] }).toBe(true)
}

test.skip(INPUT === null, 'set the five LABWEAVER_E2E_VGPU_IMAGE_* variables to run this scenario')

test('platform administrator imports one requested vGPU guest image through the UI', async ({ page }) => {
  test.setTimeout(360_000)
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
    if (!rowMatches(existingRows[0], INPUT)) throw new Error('LW_VGPU_IMAGE_BINDING_CONFLICT')
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
  await importButton.click()
  await waitForImportReadback(page, INPUT)
})
