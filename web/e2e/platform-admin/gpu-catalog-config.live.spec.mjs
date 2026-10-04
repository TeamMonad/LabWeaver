import { expect, test } from '@playwright/test'

const GPU_CATALOG_ENTRIES_ENV = 'LABWEAVER_E2E_GPU_CATALOG_ENTRIES'
const SINGLE_ENTRY_ENV = Object.freeze({
  class: 'LABWEAVER_E2E_GPU_CATALOG_CLASS',
  mode: 'LABWEAVER_E2E_GPU_CATALOG_MODE',
  providerBinding: 'LABWEAVER_E2E_GPU_CATALOG_PROVIDER_BINDING',
  capacityUnits: 'LABWEAVER_E2E_GPU_CATALOG_CAPACITY_UNITS',
  allocationBinding: 'LABWEAVER_E2E_GPU_CATALOG_ALLOCATION_BINDING',
})

const GPU_MODES = Object.freeze(['exclusive', 'container_time_slice', 'vm_vgpu'])
const GPU_MODE_LABELS = Object.freeze({
  exclusive: '独占',
  container_time_slice: '容器时间片',
  vm_vgpu: 'VM vGPU',
})

function inputError(detail) {
  throw new Error(`LW_GPU_CATALOG_CONFIG_INPUT_INVALID:${detail}`)
}

function normalizeEntry(value, index) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    inputError(`entry-${index + 1}-must-be-an-object`)
  }

  const className = typeof value.class === 'string' ? value.class.trim() : ''
  if (!/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(className)) {
    inputError(`entry-${index + 1}-class-invalid`)
  }

  const mode = typeof value.mode === 'string' ? value.mode.trim() : ''
  if (!GPU_MODES.includes(mode)) inputError(`entry-${index + 1}-mode-invalid`)

  const providerBinding = typeof value.providerBinding === 'string'
    ? value.providerBinding.trim()
    : ''
  if (!providerBinding || providerBinding.length > 120) {
    inputError(`entry-${index + 1}-provider-binding-invalid`)
  }

  const allocationBinding = typeof value.allocationBinding === 'string'
    ? value.allocationBinding.trim()
    : ''
  if (!allocationBinding || allocationBinding.length > 256) {
    inputError(`entry-${index + 1}-allocation-binding-invalid`)
  }

  const capacityUnits = typeof value.capacityUnits === 'number'
    ? value.capacityUnits
    : Number(value.capacityUnits)
  if (!Number.isInteger(capacityUnits) || capacityUnits < 1) {
    inputError(`entry-${index + 1}-capacity-units-invalid`)
  }

  return Object.freeze({
    class: className,
    mode,
    providerBinding,
    capacityUnits,
    allocationBinding,
  })
}

function configuredEntries() {
  const jsonValue = process.env[GPU_CATALOG_ENTRIES_ENV]?.trim()
  if (jsonValue) {
    let parsed
    try {
      parsed = JSON.parse(jsonValue)
    } catch {
      inputError('entries-json-invalid')
    }
    const rawEntries = Array.isArray(parsed) ? parsed : [parsed]
    const entries = rawEntries.map(normalizeEntry)
    const classes = new Set()
    for (const entry of entries) {
      if (classes.has(entry.class)) inputError('duplicate-class')
      classes.add(entry.class)
    }
    return entries
  }

  const singleEntry = Object.fromEntries(
    Object.entries(SINGLE_ENTRY_ENV).map(([field, variable]) => [field, process.env[variable]?.trim() || '']),
  )
  const presentFields = Object.entries(singleEntry).filter(([, value]) => value !== '')
  if (presentFields.length === 0) return []
  if (presentFields.length !== Object.keys(SINGLE_ENTRY_ENV).length) {
    inputError('single-entry-fields-incomplete')
  }

  return [normalizeEntry(singleEntry, 0)]
}

const ENTRIES = configuredEntries()

async function waitForCatalogSettled(page, operation) {
  await expect.poll(async () => {
    const alert = page.locator('[role="alert"]')
    if (await alert.count()) {
      const code = (await alert.first().locator('.diagnostic-code').textContent())?.trim() || 'diagnostic-missing'
      throw new Error(`LW_GPU_CATALOG_${operation}_FAILED:${code}`)
    }
    if (await page.locator('[role="region"][aria-label="GPU 目录"]').count()) return 'ready'
    if (await page.getByText('GPU 目录为空。', { exact: true }).count()) return 'empty'
    return 'loading'
  }, { timeout: 30_000, intervals: [250, 500, 1000] }).toMatch(/^(ready|empty)$/)
}

async function readCatalogRows(page) {
  const region = page.locator('[role="region"][aria-label="GPU 目录"]')
  if (await region.count() === 0) return []
  return await region.locator('tbody tr.data-table__row').evaluateAll((rowElements) => rowElements.map((row) => {
    const cells = [...row.querySelectorAll('td')].map((cell) => cell.textContent?.trim() || '')
    return {
      class: cells[0],
      modeLabel: cells[1],
      providerBinding: cells[2],
      allocationBinding: cells[3],
      capacityUnits: Number(cells[4]),
      revision: Number(cells[5]),
      active: cells[6] === '可用',
    }
  }))
}

function rowDiagnostic(row) {
  if (!row) return 'missing'
  return `revision=${row.revision};mode=${row.modeLabel};provider=${row.providerBinding};capacity=${row.capacityUnits};allocation=${row.allocationBinding};active=${row.active}`
}

function latestRowForClass(rows, entry) {
  const scopedRows = rows.filter((row) => row.class === entry.class)
  if (scopedRows.some((row) => !Number.isInteger(row.revision) || row.revision < 1)) {
    throw new Error(`LW_GPU_CATALOG_READBACK_INVALID_REVISION:${entry.class}`)
  }
  if (scopedRows.length === 0) return { current: null, nextRevision: 1 }

  const maxRevision = Math.max(...scopedRows.map((row) => row.revision))
  const latestRows = scopedRows.filter((row) => row.revision === maxRevision)
  if (latestRows.length !== 1) {
    throw new Error(`LW_GPU_CATALOG_READBACK_DUPLICATE_LATEST:${entry.class}`)
  }
  return { current: latestRows[0], nextRevision: maxRevision + 1 }
}

function rowMatches(row, entry) {
  return Boolean(
    row
      && row.active
      && row.modeLabel === GPU_MODE_LABELS[entry.mode]
      && row.providerBinding === entry.providerBinding
      && row.capacityUnits === entry.capacityUnits
      && row.allocationBinding === entry.allocationBinding,
  )
}

async function createEntryByUi(page, entry, revision) {
  const form = page.locator('section.create-card form')
  await form.getByLabel('class', { exact: true }).fill(entry.class)
  // The mode label may include the inline time-slice help text in deployed builds;
  // the create form has one select, so keep this locator tied to the visible form.
  await form.locator('select').selectOption(entry.mode)
  await form.getByLabel('provider binding', { exact: true }).fill(entry.providerBinding)
  await form.getByLabel('capacity units', { exact: true }).fill(String(entry.capacityUnits))
  await form.getByLabel('allocation binding', { exact: true }).fill(entry.allocationBinding)
  await form.getByLabel('revision', { exact: true }).fill(String(revision))
  const createButton = form.getByRole('button', { name: '创建', exact: true })
  await expect(createButton).toBeEnabled()
  await createButton.click()

  // The catalog keeps rendering its previous rows while the mutation reloads the
  // server projection. Waiting only for the table to exist can therefore read
  // the old revision and report a false mismatch. The submitted revision is
  // fixed for this attempt: a concurrent change is reported instead of silently
  // incrementing and retrying.
  await expect(createButton).toBeEnabled({ timeout: 60_000 })
  let observed = null
  let diagnosticCode = null
  let readbackFailure = null
  try {
    await expect.poll(async () => {
      const alert = page.locator('[role="alert"]')
      if (await alert.count()) {
        diagnosticCode = (await alert.first().locator('.diagnostic-code').textContent())?.trim() || 'diagnostic-missing'
        throw new Error('catalog mutation failed')
      }
      try {
        const state = latestRowForClass(await readCatalogRows(page), entry)
        observed = state.current
        return state.current?.revision === revision && rowMatches(state.current, entry)
      } catch (error) {
        readbackFailure = error instanceof Error ? error.message : String(error)
        return false
      }
    }, { timeout: 60_000, intervals: [250, 500, 1000] }).toBe(true)
  } catch {
    if (diagnosticCode) throw new Error(`LW_GPU_CATALOG_CREATE_FAILED:${diagnosticCode}`)
    if (readbackFailure) throw new Error(readbackFailure)
    throw new Error(`LW_GPU_CATALOG_READBACK_MISMATCH:${entry.class}:expected-revision=${revision}:observed=${rowDiagnostic(observed)}`)
  }
}

test.skip(ENTRIES.length === 0, `set ${GPU_CATALOG_ENTRIES_ENV} or the five single-entry variables to run this scenario`)

test('platform administrator configures explicitly requested GPU catalog entries through the UI', async ({ page }) => {
  test.setTimeout(120_000)
  // The platform-admin project supplies the existing Keycloak storage state from auth.setup.mjs.
  await page.goto('/admin/gpu-catalog', { waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: 'GPU 目录', exact: true })).toBeVisible()
  await waitForCatalogSettled(page, 'LOAD')

  for (const entry of ENTRIES) {
    const before = await readCatalogRows(page)
    const state = latestRowForClass(before, entry)
    if (!rowMatches(state.current, entry)) {
      await createEntryByUi(page, entry, state.nextRevision)
    }

    const after = await readCatalogRows(page)
    const readback = latestRowForClass(after, entry)
    if (!rowMatches(readback.current, entry)) {
      throw new Error(`LW_GPU_CATALOG_READBACK_MISMATCH:${entry.class}:observed=${rowDiagnostic(readback.current)}`)
    }
    if (state.current && rowMatches(state.current, entry) && readback.nextRevision !== state.nextRevision) {
      throw new Error(`LW_GPU_CATALOG_UNEXPECTED_WRITE:${entry.class}`)
    }
  }
})
