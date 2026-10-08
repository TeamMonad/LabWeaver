import { expect, test } from '@playwright/test'
import { expectJson, navigateFromHomeByUi, selectProjectByUi } from '../support/live.mjs'

const ENABLED = process.env.LABWEAVER_E2E_PUBLIC_SETTINGS?.trim() === '1'
const PROJECT_ID = process.env.LABWEAVER_E2E_PUBLIC_SETTINGS_PROJECT_ID?.trim() || ''
const PUBLIC_SETTINGS_TIMEOUT_MS = 180_000
const GPU_MODE_LABELS = Object.freeze({
  exclusive: '独占',
  container_time_slice: '容器时间片',
  vm_vgpu: 'VM vGPU',
})

if (ENABLED && !PROJECT_ID) throw new Error('LABWEAVER_E2E_PUBLIC_SETTINGS_PROJECT_ID_REQUIRED')
if (ENABLED && !/^[0-9a-f-]{36}$/i.test(PROJECT_ID)) {
  throw new Error('LABWEAVER_E2E_PUBLIC_SETTINGS_PROJECT_ID_INVALID')
}

test.skip(!ENABLED, 'set LABWEAVER_E2E_PUBLIC_SETTINGS=1 and LABWEAVER_E2E_PUBLIC_SETTINGS_PROJECT_ID to run this read-only public admin journey')
test.describe.configure({ retries: 0, timeout: PUBLIC_SETTINGS_TIMEOUT_MS })

function apiPath(response) {
  return new URL(response.url()).pathname
}

function installAuthorizationFailureGuard(page, baseURL) {
  const origin = new URL(baseURL).origin
  const failures = []
  page.on('response', (response) => {
    if (![401, 403].includes(response.status())) return
    const url = new URL(response.url())
    if (url.origin !== origin || !url.pathname.startsWith('/api/')) return
    failures.push(`${response.status()}:${url.pathname}`)
  })
  return () => {
    if (failures.length > 0) {
      throw new Error(`LW_PUBLIC_SETTINGS_AUTHORIZATION_FAILURE:${failures.join(',')}`)
    }
  }
}

async function readRenderedMemberUsername(page) {
  const membersSection = page.locator('.members-section')
  const rows = page.locator('.member-list .member-row')
  await expect.poll(async () => {
    const diagnostic = membersSection.locator('.diagnostic-banner--error').first()
    if (await diagnostic.count()) {
      const code = (await diagnostic.locator('.diagnostic-code').textContent())?.trim() || 'diagnostic-missing'
      const message = (await diagnostic.locator('.diagnostic-message').textContent())?.trim() || 'message-missing'
      throw new Error(`LW_PUBLIC_SETTINGS_MEMBER_LOAD_FAILED:${code}:${message}`)
    }
    return rows.count()
  }, { timeout: 60_000, intervals: [500, 1000, 2000] }).toBeGreaterThan(0)
  const username = await rows.evaluateAll((elements) => {
    for (const element of elements) {
      const text = element.textContent ?? ''
      if (!/(教师|学生)/.test(text)) continue
      const match = text.match(/账号：\s*([^·\s]+)/)
      if (match?.[1]) return match[1]
    }
    return ''
  })
  if (!username) throw new Error('LW_PUBLIC_SETTINGS_EXISTING_TEACHER_OR_STUDENT_MISSING')
  return username
}

async function searchExistingMember(page) {
  const directory = page.locator('.directory-picker')
  await expect(directory).toBeVisible({ timeout: 60_000 })
  const username = await readRenderedMemberUsername(page)
  const query = directory.getByLabel('查找组织账号', { exact: true })
  await query.fill(username)
  const searchResponse = page.waitForResponse((response) => (
    response.request().method() === 'GET'
      && apiPath(response) === '/api/v1/directory/users'
  ))
  const [response] = await Promise.all([
    searchResponse,
    directory.getByRole('button', { name: '搜索账号', exact: true }).click(),
  ])
  expect(response.ok(), 'LW_PUBLIC_SETTINGS_DIRECTORY_SEARCH_FAILED').toBe(true)
  const result = directory.locator('.directory-result').filter({ hasText: username })
  await expect(result).toHaveCount(1, { timeout: 60_000 })
  await expect(result).toBeEnabled()
  await expect(result).toContainText(username)
}

async function readAndRefreshProjectPolicy(page) {
  await expect(page).toHaveURL(new RegExp(`/researcher/ai-policy[?]projectId=${PROJECT_ID}$`))
  const projectSelect = page.locator('select[data-testid="policy-project-select"]')
  await expect(projectSelect).toHaveValue(PROJECT_ID, { timeout: 60_000 })
  await expect(page.locator('.policy-summary .state-chip')).toHaveText('已启用', { timeout: 60_000 })

  const options = await expectJson(
    await page.request.get(`/api/v1/projects/${PROJECT_ID}/llm-egress-policy-options`),
    'LW_PUBLIC_SETTINGS_LLM_OPTIONS_READ_FAILED',
  )
  const policy = await expectJson(
    await page.request.get(`/api/v1/projects/${PROJECT_ID}/llm-egress-policies/active`),
    'LW_PUBLIC_SETTINGS_LLM_POLICY_READ_FAILED',
  )
  expect(options.runtimeBinding, 'LW_PUBLIC_SETTINGS_RUNTIME_BINDING_INVALID').toBe('claude-code-production')
  expect(options.models, 'LW_PUBLIC_SETTINGS_MODEL_OPTIONS_INVALID').toEqual(
    expect.arrayContaining([expect.objectContaining({ model: 'qwen3.6:35b' })]),
  )
  expect(policy).toMatchObject({
    projectId: PROJECT_ID,
    binding: {
      runtimeBinding: 'claude-code-production',
      model: 'qwen3.6:35b',
    },
  })
  await expect(page.locator('select[data-testid="policy-model-select"]')).toHaveValue('qwen3.6:35b')

  const refreshResponse = page.waitForResponse((response) => (
    response.request().method() === 'GET'
      && apiPath(response) === `/api/v1/projects/${PROJECT_ID}/llm-egress-policy-options`
  ))
  const [response] = await Promise.all([
    refreshResponse,
    page.getByRole('button', { name: '刷新项目 AI 设置', exact: true }).click(),
  ])
  expect(response.ok(), 'LW_PUBLIC_SETTINGS_LLM_REFRESH_FAILED').toBe(true)
  await expect(page.locator('select[data-testid="policy-project-select"]')).toHaveValue(PROJECT_ID)
  await expect(page.locator('select[data-testid="policy-model-select"]')).toHaveValue('qwen3.6:35b')
  await expect(page.locator('.policy-summary .state-chip')).toHaveText('已启用')
}

async function readGpuCatalog(page) {
  await expect(page.getByRole('heading', { name: 'GPU 目录', exact: true, level: 2 })).toBeVisible()
  const region = page.getByRole('region', { name: 'GPU 目录' })
  const rows = region.locator('tbody tr.data-table__row')
  await expect.poll(async () => {
    if (await page.locator('.diagnostic-banner--error').count()) throw new Error('LW_PUBLIC_SETTINGS_GPU_CATALOG_LOAD_FAILED')
    return await rows.count()
  }, { timeout: 60_000, intervals: [500, 1000, 2000] }).toBeGreaterThan(0)
  const renderedRows = await rows.evaluateAll((elements) => elements.map((row) => {
    const cells = [...row.querySelectorAll('td')].map((cell) => cell.textContent?.trim() ?? '')
    return {
      className: cells[0],
      mode: cells[1],
      providerBinding: cells[2],
      allocationBinding: cells[3],
      capacityUnits: Number(cells[4]),
      revision: Number(cells[5]),
      status: cells[6],
      active: cells[6] === '可用',
    }
  }))
  const configuredModes = new Set()
  for (const row of renderedRows) {
    const mode = Object.entries(GPU_MODE_LABELS).find(([, label]) => label === row.mode)?.[0]
    if (!mode) throw new Error(`LW_PUBLIC_SETTINGS_GPU_MODE_INVALID:${row.mode}`)
    if (!/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(row.className)) {
      throw new Error('LW_PUBLIC_SETTINGS_GPU_CLASS_INVALID')
    }
    if (!row.providerBinding || !row.allocationBinding
      || !Number.isInteger(row.capacityUnits) || row.capacityUnits < 1
      || !Number.isInteger(row.revision) || row.revision < 1) {
      throw new Error(`LW_PUBLIC_SETTINGS_GPU_ROW_INVALID:${row.className}:${mode}`)
    }
    if (!['可用', '已停用'].includes(row.status)) {
      throw new Error(`LW_PUBLIC_SETTINGS_GPU_STATUS_INVALID:${row.className}:${mode}`)
    }
    configuredModes.add(mode)
  }
  return { rows: renderedRows.length, modes: [...configuredModes].sort() }
}

async function waitForFinanceSections(page) {
  const sections = [
    ['section.rates-card', 'ul[aria-label="资源费率列表"]', '还没有资源费率。未配置费率的用量会保留待计价状态；资源申请仍按目录、容量和审批规则处理。'],
    ['section.budget-card', '.budget-summary', '该项目还没有预算记录。'],
    ['section.charges-card', '.charge-list', '该项目暂无费用记录。'],
  ]
  for (const [sectionSelector, contentSelector, emptyText] of sections) {
    const section = page.locator(sectionSelector)
    await expect(section).toBeVisible()
    await expect.poll(async () => {
      if (await page.locator('.diagnostic-banner--error').count()) throw new Error(`LW_PUBLIC_SETTINGS_FINANCE_LOAD_FAILED:${sectionSelector}`)
      if (await section.locator('.spinner').count()) return false
      return await section.locator(contentSelector).count() > 0
        || await section.getByText(emptyText, { exact: true }).count() > 0
    }, { timeout: 60_000, intervals: [500, 1000, 2000] }).toBe(true)
  }
}

async function waitForApprovalTables(page) {
  await expect(page.getByRole('heading', { name: '资源审批与资源使用授权管理', exact: true })).toBeVisible()
  for (const label of ['资源申请列表', '资源使用授权列表']) {
    const region = page.getByRole('region', { name: label })
    await expect(region).toBeVisible()
    await expect.poll(async () => {
      if (await page.locator('.diagnostic-banner--error').count()) throw new Error(`LW_PUBLIC_SETTINGS_APPROVAL_LOAD_FAILED:${label}`)
      return await region.locator('.skeleton-row').count() === 0
        && await region.locator('tbody tr.data-table__row, .data-table__state').count() > 0
    }, { timeout: 60_000, intervals: [500, 1000, 2000] }).toBe(true)
  }
}

test('platform administrator reads public project settings and resource pages', async ({ page, baseURL }) => {
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')
  const assertNoAuthorizationFailures = installAuthorizationFailureGuard(page, baseURL)

  await navigateFromHomeByUi(page, '项目与工作空间')
  await expect(page.getByRole('heading', { name: '项目与工作空间', exact: true })).toBeVisible()
  await selectProjectByUi(page, PROJECT_ID)
  await expect(page.locator('.project-detail')).toContainText(PROJECT_ID)
  await searchExistingMember(page)

  await page.getByRole('link', { name: '项目 AI 设置', exact: true }).click()
  await readAndRefreshProjectPolicy(page)

  await navigateFromHomeByUi(page, 'GPU 目录')
  const gpuCatalog = await readGpuCatalog(page)

  await navigateFromHomeByUi(page, '预算与费用')
  await expect(page).toHaveURL(new RegExp(`/admin/resource-finance[?]projectId=${PROJECT_ID}$`))
  await expect(page.getByRole('heading', { name: '费率、预算与费用', exact: true })).toBeVisible()
  const projectSelect = page.locator('.project-strip select')
  await expect(projectSelect.locator(`option[value="${PROJECT_ID}"]`)).toHaveCount(1)
  await expect(projectSelect).toHaveValue(PROJECT_ID)
  const projectIdDetails = page.locator('details.project-id-details')
  await expect(projectIdDetails).toBeVisible()
  await projectIdDetails.locator('summary').click()
  await expect(projectIdDetails).toContainText(PROJECT_ID)
  await waitForFinanceSections(page)

  await navigateFromHomeByUi(page, '资源审批')
  await waitForApprovalTables(page)

  assertNoAuthorizationFailures()
  console.log(`[LW_PUBLIC_SETTINGS_READ_OK] project=${PROJECT_ID} gpuModes=${gpuCatalog.modes.join(',')} gpuRows=${gpuCatalog.rows}`)
})
