import { randomBytes } from 'node:crypto'
import { expect } from '@playwright/test'

export const AUTH_STATE = Object.freeze({
  teacher: '.auth/teacher.json',
  student: '.auth/student.json',
  admin: '.auth/platform-admin.json',
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

export function policyFor(projectId, courseId = null) {
  return {
    id: uuidv7(),
    projectId,
    courseId,
    revision: 1,
    binding: {
      runtimeBinding: 'claude-code-production',
      model: 'claude-sonnet-4-6-20260601',
      claudeCodeVersion: '2.1.215',
      maxInFlightPerWorker: 1,
    },
    budget: {
      maxInputTokens: 100000,
      maxOutputTokens: 20000,
      maxRequests: 8,
      maxCostMicrousd: 1000000,
      timeoutMilliseconds: 120000,
      maxTransientRetries: 1,
      maxSchemaRepairs: 2,
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

export async function createProjectPolicy(request, baseURL, projectId) {
  const body = policyFor(projectId)
  const response = await request.post(`/api/v1/projects/${projectId}/llm-egress-policies`, {
    headers: await csrfHeaders(request, baseURL, { 'Idempotency-Key': uuidv7() }),
    data: body,
  })
  return await expectJson(response, 'PROJECT_POLICY_CREATE_FAILED')
}

export async function createProjectByUi(page, name) {
  await page.goto('/researcher/workspaces', { waitUntil: 'domcontentloaded' })
  await page.getByRole('heading', { name: '项目与工作空间', exact: true }).waitFor()
  await page.getByRole('button', { name: '新建项目', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '新建项目' })
  await dialog.getByText('项目名称', { exact: true }).waitFor()
  await dialog.locator('input').first().fill(name)
  const responsePromise = page.waitForResponse((response) => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/projects')
  await dialog.getByRole('button', { name: '创建项目', exact: true }).click()
  const project = await expectJson(await responsePromise, 'PROJECT_CREATE_FAILED')
  await page.getByRole('option', { name: new RegExp(project.id) }).waitFor()
  return project
}

export async function selectProjectByUi(page, projectId) {
  const trigger = page.getByRole('button', { name: '选择项目', exact: true })
  await trigger.click()
  const dialog = page.getByRole('dialog', { name: '项目选择器' })
  const option = dialog.getByRole('option', { name: new RegExp(projectId) })
  await option.waitFor()
  await option.click()
  await expect(trigger).toContainText(projectId)
}

export async function uploadPackage(request, baseURL, projectId, policyRevision = 1) {
  const content = Buffer.from('# LabWeaver live Work fixture\n\nUse the managed environment.\n', 'utf8')
  const files = [{ path: 'README.md', sizeBytes: content.byteLength, mediaType: 'text/markdown' }]
  const sessionResponse = await request.post(`/api/v1/projects/${projectId}/problem-package-uploads`, {
    headers: await csrfHeaders(request, baseURL, { 'Idempotency-Key': uuidv7() }),
    data: { projectId, courseId: null, files, retentionPolicyRevision: policyRevision },
  })
  const session = await expectJson(sessionResponse, 'PACKAGE_UPLOAD_SESSION_FAILED')
  const sessionEtag = sessionResponse.headers().etag
  if (!/^"rev-\d+"$/.test(sessionEtag ?? '')) throw new Error(`PACKAGE_UPLOAD_ETAG_INVALID:${sessionEtag ?? 'missing'}`)
  const target = session.uploadTargets.find((item) => item.path === 'README.md')
  if (!target) throw new Error('PACKAGE_UPLOAD_TARGET_MISSING:README.md')
  const uploadResponse = await request.put(target.uploadUrl, { headers: target.requiredHeaders, data: content })
  if (!uploadResponse.ok()) throw new Error(`PACKAGE_OBJECT_UPLOAD_FAILED:${uploadResponse.status()} ${(await uploadResponse.text()).slice(0, 2000)}`)
  const completeResponse = await request.post(`/api/v1/projects/${projectId}/problem-package-uploads/${session.id}/complete`, {
    headers: await csrfHeaders(request, baseURL, { 'Idempotency-Key': uuidv7(), 'If-Match': sessionEtag }),
    data: {},
  })
  return await expectJson(completeResponse, 'PACKAGE_UPLOAD_COMPLETE_FAILED')
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
