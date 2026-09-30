import { expect, test } from '@playwright/test'

const INPUT_ENV = Object.freeze({
  gpuClass: 'LABWEAVER_E2E_RESOURCE_RATE_GPU_CLASS',
  gpuMode: 'LABWEAVER_E2E_RESOURCE_RATE_GPU_MODE',
  currency: 'LABWEAVER_E2E_RESOURCE_RATE_CURRENCY',
  amount: 'LABWEAVER_E2E_RESOURCE_RATE_AMOUNT',
})

const GPU_MODES = Object.freeze(['exclusive', 'container_time_slice', 'vm_vgpu'])
const GPU_MODE_LABELS = Object.freeze({
  exclusive: '独占',
  container_time_slice: '容器时间片',
  vm_vgpu: 'VM vGPU',
})
const GPU_UNIT = 'gpu_unit_second'
const UNIT_QUANTITY = 1

function inputError(detail) {
  throw new Error(`LW_RESOURCE_RATE_CONFIG_INPUT_INVALID:${detail}`)
}

function configuredInput() {
  const input = Object.fromEntries(
    Object.entries(INPUT_ENV).map(([field, variable]) => [field, process.env[variable]?.trim() || '']),
  )
  const presentFields = Object.entries(input).filter(([, value]) => value !== '')
  if (presentFields.length === 0) return null
  if (presentFields.length !== Object.keys(INPUT_ENV).length) inputError('required-fields-incomplete')

  if (!/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(input.gpuClass)) {
    inputError('gpu-class-invalid')
  }
  if (!GPU_MODES.includes(input.gpuMode)) inputError('gpu-mode-invalid')
  if (!/^[A-Za-z0-9_-]{1,32}$/.test(input.currency)) inputError('currency-invalid')
  if (!/^(0|[1-9][0-9]*)\.[0-9]{6}$/.test(input.amount) || Number(input.amount) <= 0) {
    inputError('amount-invalid')
  }

  return Object.freeze({
    gpuClass: input.gpuClass,
    gpuMode: input.gpuMode,
    currency: input.currency,
    amount: input.amount,
  })
}

const INPUT = configuredInput()

function rateLabel(input) {
  return `GPU ${input.gpuClass} · ${GPU_MODE_LABELS[input.gpuMode]}`
}

function rowMatches(row, input, revision = null) {
  return Boolean(
    row
      && row.label === rateLabel(input)
      && row.detail.startsWith(`${UNIT_QUANTITY} 基础单位 · ${input.amount} ${input.currency} ·`)
      && Number.isInteger(row.revision)
      && row.revision >= 1
      && (revision === null || row.revision === revision),
  )
}

function currentRateDimension(rate, input, now = Date.now()) {
  const effectiveFrom = Date.parse(rate?.effectiveFrom)
  const effectiveUntil = rate?.effectiveUntil == null ? Number.POSITIVE_INFINITY : Date.parse(rate.effectiveUntil)
  return Boolean(
    rate
      && rate.unit === GPU_UNIT
      && rate.gpuClass === input.gpuClass
      && rate.gpuMode === input.gpuMode
      && rate.unitQuantity === UNIT_QUANTITY
      && Number.isFinite(effectiveFrom)
      && effectiveFrom <= now
      && effectiveUntil > now,
  )
}

function rateMatchesInput(rate, input) {
  return currentRateDimension(rate, input)
    && rate.unitPrice?.currency === input.currency
    && rate.unitPrice?.amount === input.amount
}

async function readCurrentRates(page) {
  const response = await page.request.get('/api/v1/resource/rates')
  if (!response.ok()) throw new Error(`LW_RESOURCE_RATE_LIST_FAILED:status-${response.status()}`)
  const rates = await response.json()
  if (!Array.isArray(rates)) throw new Error('LW_RESOURCE_RATE_LIST_INVALID')
  return rates
}

async function readRateRows(page) {
  const list = page.locator('section.rates-card ul[aria-label="资源费率列表"]')
  if (await list.count() === 0) return []
  return list.locator('li.rate-row').evaluateAll((rowElements) => rowElements.map((row) => {
    const label = row.querySelector('.rate-main strong')?.textContent?.trim() || ''
    const detail = row.querySelector('.rate-main small')?.textContent?.trim() || ''
    const revisionText = row.querySelector('.state-chip')?.textContent || ''
    const revision = Number(revisionText.match(/版本\s+(\d+)/)?.[1] || NaN)
    return { label, detail, revision }
  }))
}

async function rateDiagnosticCode(page) {
  const banners = page.locator('.diagnostic-banner')
  const count = await banners.count()
  for (let index = 0; index < count; index += 1) {
    const banner = banners.nth(index)
    const code = (await banner.locator('.diagnostic-code').textContent())?.trim() || ''
    if (code.startsWith('RESOURCE_RATE')) return code
  }
  return 'diagnostic-missing'
}

async function waitForRateListSettled(page, operation) {
  const ratesCard = page.locator('section.rates-card')
  await expect.poll(async () => {
    const errorBanner = ratesCard.locator('[role="alert"]')
    if (await errorBanner.count()) {
      const code = (await errorBanner.first().locator('.diagnostic-code').textContent())?.trim() || 'diagnostic-missing'
      throw new Error(`LW_RESOURCE_RATE_${operation}_FAILED:${code}`)
    }
    if (await ratesCard.locator('ul[aria-label="资源费率列表"]').count()) return 'ready'
    if (await ratesCard.getByText('还没有资源费率。创建费率后，匹配的 GPU 目录项才能用于资源申请。', { exact: true }).count()) return 'empty'
    return 'loading'
  }, { timeout: 60_000, intervals: [250, 500, 1000] }).toMatch(/^(ready|empty)$/)
}

function rowDescription(row) {
  if (!row) return 'missing'
  return `label=${row.label};detail=${row.detail};revision=${row.revision}`
}

async function waitForRateReadback(page, input, revision, operation) {
  let observed = null
  try {
    await expect.poll(async () => {
      const rows = await readRateRows(page)
      observed = rows.find((row) => rowMatches(row, input, revision)) || null
      return Boolean(observed)
    }, { timeout: 60_000, intervals: [250, 500, 1000] }).toBe(true)
  } catch {
    const diagnostic = await rateDiagnosticCode(page)
    if (diagnostic !== 'diagnostic-missing') throw new Error(`LW_RESOURCE_RATE_${operation}_FAILED:${diagnostic}`)
    throw new Error(`LW_RESOURCE_RATE_READBACK_MISMATCH:${input.gpuClass}:expected-revision=${revision}:observed=${rowDescription(observed)}`)
  }
}

async function createRateByUi(page, input) {
  const form = page.getByTestId('resource-rate-form')
  await form.locator('select').first().selectOption(GPU_UNIT)
  await form.getByLabel('GPU class', { exact: true }).fill(input.gpuClass)
  await form.getByLabel('分配模式', { exact: true }).selectOption(input.gpuMode)
  await form.getByLabel('每次计费基础单位数', { exact: true }).fill(String(UNIT_QUANTITY))
  await form.getByLabel('单价', { exact: true }).fill(input.amount)
  await form.getByLabel('币种', { exact: true }).fill(input.currency)

  const createButton = form.getByRole('button', { name: '创建费率版本', exact: true })
  await expect(createButton).toBeEnabled()
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST' && url.pathname === '/api/v1/resource/rates'
  })
  await createButton.click()
  const response = await responsePromise
  if (!response.ok()) throw new Error(`LW_RESOURCE_RATE_CREATE_FAILED:status-${response.status()}`)
}

test.skip(INPUT === null, `set the four ${Object.values(INPUT_ENV).join(', ')} variables to run this scenario`)

test('platform administrator configures one explicitly requested GPU rate through the UI', async ({ page }) => {
  test.setTimeout(120_000)
  if (!INPUT) throw new Error('LW_RESOURCE_RATE_INPUT_REQUIRED')

  await page.goto('/admin/resource-finance', { waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: '预算与费用', exact: true })).toBeVisible()
  await waitForRateListSettled(page, 'LOAD')

  const before = await readCurrentRates(page)
  const current = before.filter((rate) => currentRateDimension(rate, INPUT))
  if (current.length > 1) throw new Error(`LW_RESOURCE_RATE_ACTIVE_AMBIGUOUS:${INPUT.gpuClass}`)
  if (current.length === 1) {
    if (!rateMatchesInput(current[0], INPUT)) {
      throw new Error(`LW_RESOURCE_RATE_ACTIVE_CONFLICT:${INPUT.gpuClass}`)
    }
    await waitForRateReadback(page, INPUT, current[0].revision, 'READBACK')
    return
  }

  await createRateByUi(page, INPUT)
  const after = await readCurrentRates(page)
  const created = after.filter((rate) => currentRateDimension(rate, INPUT))
  if (created.length !== 1 || !rateMatchesInput(created[0], INPUT)) {
    throw new Error(`LW_RESOURCE_RATE_ACTIVE_READBACK_MISMATCH:${INPUT.gpuClass}`)
  }
  await waitForRateReadback(page, INPUT, created[0].revision, 'CREATE')
})
