import { expect, test } from '@playwright/test'
import { navigateFromHomeByUi } from '../support/live.mjs'

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
      && row.details.some((detail) => detail.startsWith(`${UNIT_QUANTITY} 基础单位 · ${input.amount} ${input.currency} ·`))
      && row.details.includes('基础单位：GPU 分配单位秒')
      && row.details.some((detail) => detail.includes(`${input.amount} ${input.currency} / GPU 分配单位秒`))
      && row.versionState === '现行'
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
    const details = Array.from(row.querySelectorAll('.rate-main small'), (detail) => detail.textContent?.trim() || '')
    const revisionText = row.querySelector('.state-chip')?.textContent || ''
    const revision = Number(revisionText.match(/版本\s+(\d+)/)?.[1] || NaN)
    const versionState = revisionText.split('·')[0].trim()
    return { label, details, revision, versionState }
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
  return `label=${row.label};details=${row.details.join(';')};revision=${row.revision};state=${row.versionState}`
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
  await form.getByLabel('计费单位', { exact: true }).selectOption(GPU_UNIT)
  const gpuSelect = form.getByLabel('GPU 目录分配类型', { exact: true })
  await expect(gpuSelect).toBeEnabled()
  const gpuOption = `${input.gpuClass}:${input.gpuMode}`
  await expect(gpuSelect.locator(`option[value="${gpuOption}"]`)).toHaveText(`${input.gpuClass} · ${GPU_MODE_LABELS[input.gpuMode]}`)
  await gpuSelect.selectOption(gpuOption)
  await form.getByLabel('费率单价', { exact: true }).fill(input.amount)
  await form.getByLabel('币种', { exact: true }).fill(input.currency)
  // The UI only accepts future versions. Select the next minute with enough
  // time to submit, then observe that same version becoming current.
  const effectiveFrom = await page.evaluate(() => {
    const date = new Date(Math.ceil((Date.now() + 10_000) / 60_000) * 60_000)
    const pad = (value) => String(value).padStart(2, '0')
    return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
  })
  await form.getByLabel('生效时间', { exact: true }).fill(effectiveFrom)
  await expect(form.getByRole('status')).toContainText(`实际提交：${UNIT_QUANTITY} GPU 分配单位秒，金额 ${input.amount} ${input.currency}`)

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

  await navigateFromHomeByUi(page, '预算与费用')
  await expect(page.getByRole('heading', { name: '费率、预算与费用', exact: true })).toBeVisible()
  await waitForRateListSettled(page, 'LOAD')

  const before = await readCurrentRates(page)
  const upcoming = before.filter((rate) => rate.unit === GPU_UNIT
    && rate.gpuClass === INPUT.gpuClass && rate.gpuMode === INPUT.gpuMode
    && rate.unitQuantity === UNIT_QUANTITY && Date.parse(rate.effectiveFrom) > Date.now())
  if (upcoming.length > 1) throw new Error(`LW_RESOURCE_RATE_FUTURE_AMBIGUOUS:${INPUT.gpuClass}`)
  if (upcoming.length === 1 && (upcoming[0].unitPrice?.currency !== INPUT.currency
    || upcoming[0].unitPrice?.amount !== INPUT.amount)) {
    throw new Error(`LW_RESOURCE_RATE_FUTURE_CONFLICT:${INPUT.gpuClass}`)
  }
  const current = before.filter((rate) => currentRateDimension(rate, INPUT))
  if (current.length > 1) throw new Error(`LW_RESOURCE_RATE_ACTIVE_AMBIGUOUS:${INPUT.gpuClass}`)
  if (current.length === 1) {
    if (!rateMatchesInput(current[0], INPUT)) {
      throw new Error(`LW_RESOURCE_RATE_ACTIVE_CONFLICT:${INPUT.gpuClass}`)
    }
    await waitForRateReadback(page, INPUT, current[0].revision, 'READBACK')
    return
  }

  if (upcoming.length === 0) await createRateByUi(page, INPUT)
  let after = []
  await expect.poll(async () => {
    after = await readCurrentRates(page)
    return after.some((rate) => rateMatchesInput(rate, INPUT)
      && (upcoming.length === 0 || rate.id === upcoming[0].id))
  }, { timeout: 75_000, intervals: [250, 500, 1000] }).toBe(true)
  const created = after.filter((rate) => currentRateDimension(rate, INPUT))
  if (created.length !== 1 || !rateMatchesInput(created[0], INPUT)) {
    throw new Error(`LW_RESOURCE_RATE_ACTIVE_READBACK_MISMATCH:${INPUT.gpuClass}`)
  }
  await waitForRateReadback(page, INPUT, created[0].revision, 'CREATE')
})
