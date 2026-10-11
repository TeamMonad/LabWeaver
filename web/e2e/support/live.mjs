import { randomBytes } from 'node:crypto'
import path from 'node:path'
import { expect } from '@playwright/test'

const authDir = process.env.LABWEAVER_AUTH_DIR || '.auth'
export const AUTH_STATE = Object.freeze({
  teacher: path.join(authDir, 'teacher.json'),
  student: path.join(authDir, 'student.json'),
  admin: path.join(authDir, 'platform-admin.json'),
})

export function uuidv7() {
  const bytes = randomBytes(16)
  const timestamp = BigInt(Date.now())
  bytes[0] = Number((timestamp >> 40n) & 0xffn)
  bytes[1] = Number((timestamp >> 32n) & 0xffn)
  bytes[2] = Number((timestamp >> 24n) & 0xffn)
  bytes[3] = Number((timestamp >> 16n) & 0xffn)
  bytes[4] = Number((timestamp >> 8n) & 0xffn)
  bytes[5] = Number(timestamp & 0xffn)
  bytes[6] = (bytes[6] & 0x0f) | 0x70
  bytes[8] = (bytes[8] & 0x3f) | 0x80
  return [...bytes].map((value, index) => `${value.toString(16).padStart(2, '0')}${[3, 5, 7, 9].includes(index) ? '-' : ''}`).join('').slice(0, 36)
}

export function policyFor(projectId, courseId = null, providerModel, budgetOverrides = {}) {
  const model = typeof providerModel === 'string' ? providerModel.trim() : ''
  if (!model || /\s/.test(model)) {
    throw new Error('LABWEAVER_E2E_PROVIDER_MODEL_REQUIRED')
  }
  return {
    id: uuidv7(),
    projectId,
    courseId,
    revision: 1,
    binding: {
      runtimeBinding: 'claude-code-production',
      model,
      claudeCodeVersion: '2.1.215',
      maxInFlightPerWorker: 1,
    },
    budget: {
      // An authoring candidate runs for up to sixty provider turns and each turn
      // re-sends the reviewed prompt, so the ceilings cover a whole multi-turn
      // session rather than a single short request. They are deliberately far
      // above what sixty turns can consume: the binding limits are the CLI's
      // --max-turns and the wall clock, not the token accounting, which counts
      // cached prompt prefixes on every turn. A project's real policy comes from
      // the teacher-facing form; these are the acceptance harness defaults.
      maxInputTokens: Number(process.env.LABWEAVER_E2E_LLM_MAX_INPUT_TOKENS) || 20000000,
      maxOutputTokens: Number(process.env.LABWEAVER_E2E_LLM_MAX_OUTPUT_TOKENS) || 5000000,
      maxRequests: Number(process.env.LABWEAVER_E2E_LLM_MAX_REQUESTS) || 200,
      maxCostMicrousd: Number(process.env.LABWEAVER_E2E_LLM_MAX_COST_MICROUSD) || 500000000,
      timeoutMilliseconds: Number(process.env.LABWEAVER_E2E_LLM_TIMEOUT_MS) || 900000,
      maxTransientRetries: 1,
      maxSchemaRepairs: Number(process.env.LABWEAVER_E2E_LLM_MAX_SCHEMA_REPAIRS) || 2,
      ...budgetOverrides,
    },
    deniedDataClasses: [
      'secret',
      'token',
      'private_key',
      'personally_identifiable_information',
      'unallowlisted_student_submission',
    ],
    studentContentMode: 'manifest_allowlist_only',
    activatedAt: new Date().toISOString(),
  }
}

export async function csrfHeaders(request, baseURL, extra = {}) {
  const response = await request.get('/api/v1/auth/csrf')
  const body = await expectJson(response, 'CSRF_TOKEN_LOOKUP_FAILED')
  if (typeof body.csrfToken !== 'string' || body.csrfToken.length === 0) throw new Error('CSRF_TOKEN_INVALID')
  return { Origin: new URL(baseURL).origin, 'X-CSRF-Token': body.csrfToken, ...extra }
}

export async function expectJson(response, label) {
  const text = await response.text()
  if (!response.ok()) throw new Error(`${label}:${response.status()} ${text.slice(0, 2000)}`)
  try {
    return JSON.parse(text)
  } catch (error) {
    throw new Error(`${label}:invalid JSON`, { cause: error })
  }
}

function formatPolicyDecimal(value, scale) {
  const integer = Number(value)
  if (!Number.isSafeInteger(integer) || integer <= 0) throw new Error('POLICY_BUDGET_VALUE_INVALID')
  const unit = 10 ** scale
  const whole = Math.floor(integer / unit)
  const fraction = integer % unit
  if (fraction === 0) return String(whole)
  return `${whole}.${String(fraction).padStart(scale, '0').replace(/0+$/, '')}`
}

export async function configureProjectPolicyByUi(page, projectId, budgetOverrides = {}) {
  const model = process.env.LABWEAVER_E2E_PROVIDER_MODEL?.trim()
  if (!model || /\s/.test(model)) throw new Error('LABWEAVER_E2E_PROVIDER_MODEL_REQUIRED')
  const defaults = policyFor(projectId, null, model, budgetOverrides).budget
  await page.goto(`/researcher/ai-policy?projectId=${encodeURIComponent(projectId)}`, {
    waitUntil: 'domcontentloaded',
  })
  await expect(page.getByRole('heading', { name: '项目 AI 设置', exact: true })).toBeVisible()
  const projectSelect = page.locator('select[data-testid="policy-project-select"]')
  await expect(projectSelect).toBeVisible()
  await projectSelect.selectOption(projectId)
  await expect(page.locator('[data-testid="policy-options-state"]')).toBeVisible()
  const modelSelect = page.locator('select[data-testid="policy-model-select"]')
  await expect(modelSelect).toBeVisible({ timeout: 30_000 })
  await modelSelect.selectOption(model)
  const consent = page.locator('input[data-testid="policy-material-consent"]')
  await consent.check()
  const fields = {
    maxInputTokens: defaults.maxInputTokens,
    maxOutputTokens: defaults.maxOutputTokens,
    maxRequests: defaults.maxRequests,
    maxCostDollars: formatPolicyDecimal(defaults.maxCostMicrousd, 6),
    timeoutSeconds: formatPolicyDecimal(defaults.timeoutMilliseconds, 3),
    maxTransientRetries: defaults.maxTransientRetries,
    maxSchemaRepairs: defaults.maxSchemaRepairs,
  }
  for (const [name, value] of Object.entries(fields)) {
    await page.locator(`input[name="${name}"]`).fill(String(value))
  }
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/projects/${projectId}/llm-egress-policies`
  })
  await page.locator('[data-testid="policy-save-button"]').click()
  return await expectJson(await responsePromise, 'PROJECT_POLICY_UI_SAVE_FAILED')
}

/** Enter a role workbench through the public home task cards. */
export async function navigateFromHomeByUi(page, taskLabel) {
  await page.goto('/', { waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: '欢迎进入 LabWeaver', exact: true })).toBeVisible({ timeout: 60_000 })
  const tasks = page.locator('.task-grid a.task-card')
  const compact = (value) => String(value ?? '').replace(/\s+/g, '')
  let matchingIndexes = []
  await expect.poll(
    async () => {
      matchingIndexes = await tasks.evaluateAll((cards, expected) => cards.reduce((indexes, card, index) => {
        const title = card.querySelector('.card-title')?.textContent ?? ''
        if (title.replace(/\s+/g, '') === expected) indexes.push(index)
        return indexes
      }, []), compact(taskLabel))
      return matchingIndexes.length
    },
    { timeout: 60_000, intervals: [250, 500, 1000] },
  ).toBe(1)
  if (matchingIndexes.length !== 1) throw new Error(`HOME_TASK_CARD_NOT_UNIQUE:${taskLabel}`)
  await tasks.nth(matchingIndexes[0]).click()
}

export async function createProjectByUi(page, name) {
  await navigateFromHomeByUi(page, '项目与工作空间')
  await page.getByRole('heading', { name: '项目与工作空间', exact: true }).waitFor()
  await page.getByRole('button', { name: '新建项目', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '新建项目' })
  await dialog.getByText('项目名称', { exact: true }).waitFor()
  await dialog.locator('input').first().fill(name)
  const responsePromise = page.waitForResponse((response) => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/projects')
  await dialog.getByRole('button', { name: '创建项目', exact: true }).click()
  const project = await expectJson(await responsePromise, 'PROJECT_CREATE_FAILED')
  const trigger = page.getByRole('button', { name: '选择项目', exact: true })
  await expect(trigger.locator('.trigger-primary')).toHaveText(project.name, { timeout: 30_000 })
  return project
}

export async function selectProjectByUi(page, projectId) {
  const projects = await expectJson(await page.request.get('/api/v1/projects'), 'PROJECT_LIST_FOR_SELECTOR_FAILED')
  if (!Array.isArray(projects)) throw new Error('PROJECT_LIST_FOR_SELECTOR_INVALID')
  const project = projects.find((item) => item?.id === projectId)
  if (!project || typeof project.name !== 'string' || project.name.trim() === '') {
    throw new Error(`PROJECT_SELECTOR_PROJECT_NOT_FOUND:${projectId}`)
  }
  const trigger = page.getByRole('button', { name: '选择项目', exact: true })
  await trigger.click()
  const dialog = page.getByRole('dialog', { name: '项目选择器' })
  const option = dialog.locator('button.project-item').filter({ hasText: project.name })
  await expect(option).toHaveCount(1, { timeout: 30_000 })
  await option.click()
  await expect(trigger.locator('.trigger-primary')).toHaveText(project.name)
}

export async function pollJson(request, path, predicate, label, timeout = 180_000) {
  let latest
  await expect.poll(async () => {
    const response = await request.get(path)
    latest = await expectJson(response, label)
    return predicate(latest)
  }, { timeout, intervals: [1000, 2000, 3000] }).toBe(true)
  return latest
}

export async function pollEnvironmentCandidate(request, projectId, candidateId, predicate, label, timeout = 180_000) {
  if (!projectId || !candidateId) throw new Error(`${label}:candidate context missing`)
  const path = `/api/v1/projects/${projectId}/environment-candidates/${candidateId}`
  let latest
  let fatalError
  await expect.poll(async () => {
    let response
    try {
      response = await request.get(path)
    } catch (error) {
      fatalError = new Error(`${label}:request failed`, { cause: error })
      return true
    }

    const bodyText = await response.text()
    let body
    try {
      body = JSON.parse(bodyText)
    } catch (error) {
      fatalError = new Error(`${label}:invalid JSON`, { cause: error })
      return true
    }
    if (response.status() === 404 && body?.diagnosticCode === 'LW_CANDIDATE_NOT_FOUND') return false
    if (!response.ok()) {
      fatalError = new Error(`${label}:${response.status()} ${bodyText.slice(0, 2000)}`)
      return true
    }

    latest = body
    try {
      return predicate(latest)
    } catch (error) {
      fatalError = new Error(`${label}:response invalid`, { cause: error })
      return true
    }
  }, { timeout, intervals: [1000, 2000, 3000] }).toBe(true)
  if (fatalError) throw fatalError
  return latest
}
