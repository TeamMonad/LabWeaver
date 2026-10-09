import { expect } from '@playwright/test'
import { expectJson, navigateFromHomeByUi, pollJson } from './live.mjs'

function clipboardMismatchDiagnostic(expected, actual, alias, pageHasFocus) {
  const firstDifference = (() => {
    const limit = Math.min(expected.length, actual.length)
    for (let index = 0; index < limit; index += 1) {
      if (expected[index] !== actual[index]) return index
    }
    return expected.length === actual.length ? 'none' : limit
  })()
  const normalizedExpected = expected.replace(/\r\n/g, '\n')
  const normalizedActual = actual.replace(/\r\n/g, '\n')
  const category = expected === actual
    ? 'none'
    : !expected
      ? 'expected-empty'
      : !actual
        ? 'actual-empty'
        : normalizedExpected === normalizedActual
          ? 'line-ending'
          : expected.trim() === actual.trim()
            ? 'outer-whitespace'
            : 'content'
  const categoryToken = category.toUpperCase().replaceAll('-', '_')
  const firstDifferenceToken = firstDifference === 'none' ? 'NONE' : String(firstDifference)
  return [
    `WORK_SSH_CLIPBOARD_CATEGORY_${categoryToken}`,
    `WORK_SSH_CLIPBOARD_FOCUS_${pageHasFocus ? 'TRUE' : 'FALSE'}`,
    `WORK_SSH_CLIPBOARD_EXPECTED_LENGTH_${expected.length}`,
    `WORK_SSH_CLIPBOARD_ACTUAL_LENGTH_${actual.length}`,
    `WORK_SSH_CLIPBOARD_FIRST_DIFFERENCE_${firstDifferenceToken}`,
    `WORK_SSH_CLIPBOARD_EXPECTED_ALIAS_${expected.includes(alias) ? 'TRUE' : 'FALSE'}`,
    `WORK_SSH_CLIPBOARD_ACTUAL_ALIAS_${actual.includes(alias) ? 'TRUE' : 'FALSE'}`,
  ].join(':')
}

export async function waitForActiveAccessGrant(request, grantId) {
  const grant = await pollJson(
    request,
    `/api/v1/access-grants/${grantId}`,
    (value) => ['active', 'denied', 'expired', 'revoked'].includes(value.state),
    'WORK_ACCESS_GRANT_STATUS_FAILED',
    120_000,
  )
  if (grant.state !== 'active') {
    throw new Error(`WORK_ACCESS_GRANT_NOT_ACTIVE:${grant.state}:${grant.reasonCode ?? 'reason missing'}`)
  }
  return grant
}

export async function addSshPublicKeyByUi(page, identity, onAccepted = () => {}) {
  if (page.url() === 'about:blank') await navigateFromHomeByUi(page, 'SSH 公钥')
  else await page.goto('/student/ssh-keys', { waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: 'SSH 公钥', exact: true })).toBeVisible()
  await page.getByLabel('OpenSSH 公钥', { exact: true }).fill(identity.publicKeyOpenssh)
  const createResponsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST' && url.pathname === '/api/v1/me/ssh-public-keys'
  })
  await page.getByRole('button', { name: '添加', exact: true }).click()
  const created = await expectJson(await createResponsePromise, 'WORK_SSH_KEY_CREATE_FAILED')
  if (typeof created?.id !== 'string' || created.id === '') {
    throw new Error('WORK_SSH_KEY_CREATE_ID_MISSING')
  }
  onAccepted(created)
  expect(created).toMatchObject({
    id: expect.any(String),
    algorithm: 'ed25519',
    fingerprintSha256: identity.fingerprintSha256,
  })
  const fingerprintRow = page.locator(`code[title="${identity.fingerprintSha256}"]`)
  await expect(fingerprintRow).toBeVisible({ timeout: 30_000 })
  await expect(fingerprintRow).toHaveAttribute('title', identity.fingerprintSha256, { timeout: 30_000 })
  return created
}

export async function deleteSshPublicKeyByUi(page, key) {
  await page.goto('/student/ssh-keys', { waitUntil: 'domcontentloaded' })
  const fingerprintCell = page.locator(`code[title="${key.fingerprintSha256}"]`)
  await expect(fingerprintCell).toBeVisible({ timeout: 30_000 })
  await expect(fingerprintCell).toHaveAttribute('title', key.fingerprintSha256, { timeout: 30_000 })
  const row = fingerprintCell.locator('xpath=ancestor::tr[1]')
  await row.getByRole('button', { name: '删除', exact: true }).click()
  const dialog = page.getByRole('alertdialog', { name: '删除 SSH 公钥', exact: true })
  await expect(dialog).toBeVisible()
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'DELETE'
      && url.pathname === `/api/v1/me/ssh-public-keys/${encodeURIComponent(key.id)}`
  })
  await dialog.getByRole('button', { name: '删除', exact: true }).click()
  const response = await responsePromise
  if (!response.ok()) throw new Error(`WORK_SSH_KEY_DELETE_FAILED:${response.status()}`)
  await expect(fingerprintCell).toHaveCount(0, { timeout: 30_000 })
}

export async function issueEnvironmentAccessGrantByUi(
  page,
  projectId,
  environment,
  protocol = 'ssh',
  onClipboardFailure,
) {
  if (!['ssh', 'http', 'https'].includes(protocol)) throw new Error(`WORK_ACCESS_GRANT_PROTOCOL_UNSUPPORTED:${protocol}`)
  await page.goto(`/student/environments?projectId=${encodeURIComponent(projectId)}&environmentId=${encodeURIComponent(environment.id)}`, {
    waitUntil: 'domcontentloaded',
  })
  await expect(
    page.locator('.resource-title-row').getByRole('heading', {
      name: environment.displayLabel,
      exact: true,
    }),
  ).toBeVisible({ timeout: 120_000 })
  await expect(page.locator('.environment-id-details code')).toHaveText(environment.id, { timeout: 120_000 })

  const activePage = await expectJson(
    await page.request.get(`/api/v1/environments/${environment.id}/access-grants?state=active&includeTerminal=false&limit=2`),
    'WORK_SSH_ACTIVE_GRANTS_READ_FAILED',
  )
  if (!Array.isArray(activePage.items) || activePage.items.length > 1) {
    throw new Error('WORK_SSH_ACTIVE_GRANTS_AMBIGUOUS')
  }
  let grant
  if (activePage.items.length === 1) {
    grant = await expectJson(
      await page.request.get(`/api/v1/access-grants/${encodeURIComponent(activePage.items[0].id)}`),
      'WORK_SSH_ACCESS_GRANT_READ_FAILED',
    )
    if (grant.environmentRevision !== environment.revision) {
      throw new Error('WORK_SSH_ACTIVE_GRANT_REVISION_STALE')
    }
  } else {
    const createButton = page.getByRole('button', { name: '签发访问授权', exact: true })
    await expect(createButton).toBeEnabled({ timeout: 120_000 })
    const createResponsePromise = page.waitForResponse((response) => {
      const url = new URL(response.url())
      return response.request().method() === 'POST'
        && url.pathname === `/api/v1/environments/${environment.id}/access-grants`
    })
    await createButton.click()
    const accepted = await expectJson(await createResponsePromise, 'WORK_SSH_ACCESS_GRANT_CREATE_FAILED')
    expect(accepted).toMatchObject({
      id: expect.any(String),
      projectId,
      environmentId: environment.id,
      environmentRevision: environment.revision,
      state: 'requested',
    })
    grant = await waitForActiveAccessGrant(page.request, accepted.id)
  }
  expect(grant).toMatchObject({
    projectId,
    environmentId: environment.id,
    environmentRevision: environment.revision,
    state: 'active',
    endpointGrants: expect.any(Array),
  })
  const endpointGrants = grant.endpointGrants.filter((item) => {
    const protocolMatches = protocol === 'http'
      ? item.protocol === 'http' || item.protocol === 'https'
      : item.protocol === protocol
    return protocolMatches && item.health === 'healthy'
  })
  if (endpointGrants.length !== 1) throw new Error(`WORK_ACCESS_GRANT_ENDPOINT_INVALID:${protocol}`)
  await expect(page.locator('.grant-card')).toContainText(grant.id, { timeout: 30_000 })
  if (protocol === 'ssh') {
    await expect(page.locator('.ssh-command__text')).toContainText(endpointGrants[0].alias, { timeout: 30_000 })
    await expect(page.locator('.ssh-meta')).toContainText(endpointGrants[0].sshGatewayHostKeyFingerprint)
    const command = (await page.locator('.ssh-command__text').textContent())?.trim() || ''
    if (!command) throw new Error('WORK_SSH_COMMAND_TEXT_MISSING')
    await page.context().grantPermissions(['clipboard-read', 'clipboard-write'], {
      origin: new URL(page.url()).origin,
    })
    await page.bringToFront()
    const pageHasFocus = await page.evaluate(() => document.hasFocus())
    const copyButton = page.getByRole('button', { name: '复制 SSH 命令', exact: true })
    await expect(copyButton).toBeEnabled({ timeout: 30_000 })
    await copyButton.click()
    await expect(copyButton).toContainText('已复制', { timeout: 5_000 })
    const copiedCommand = await page.evaluate(() => navigator.clipboard.readText())
    if (copiedCommand !== command) {
      const clipboardError = new Error(
        `WORK_SSH_COMMAND_CLIPBOARD_MISMATCH:${clipboardMismatchDiagnostic(command, copiedCommand, endpointGrants[0].alias, pageHasFocus)}`,
      )
      if (typeof onClipboardFailure === 'function') onClipboardFailure(clipboardError)
      else throw clipboardError
    }
  }
  return { grant, endpointGrant: endpointGrants[0] }
}

export async function issueEnvironmentSshAccessGrantByUi(page, projectId, environment, onClipboardFailure) {
  return await issueEnvironmentAccessGrantByUi(page, projectId, environment, 'ssh', onClipboardFailure)
}

/** Revoke the currently rendered access grant through the environment page. */
export async function revokeEnvironmentAccessGrantByUi(page, projectId, environmentId, grantId) {
  await page.goto(`/student/environments?projectId=${encodeURIComponent(projectId)}&environmentId=${encodeURIComponent(environmentId)}`, {
    waitUntil: 'domcontentloaded',
  })
  await expect(page.getByRole('heading', { name: '项目环境控制台', exact: true })).toBeVisible({ timeout: 120_000 })
  const card = page.locator('.grant-card')
  await expect(card).toBeVisible({ timeout: 120_000 })
  await expect(card).toContainText(grantId, { timeout: 30_000 })
  const revokeButton = page.getByRole('button', { name: '撤销授权', exact: true })
  await expect(revokeButton).toBeEnabled({ timeout: 30_000 })
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/access-grants/${grantId}/revoke`
  })
  await revokeButton.click()
  const accepted = await expectJson(await responsePromise, 'WORK_ACCESS_GRANT_REVOKE_FAILED')
  expect(accepted).toMatchObject({ id: grantId })
  const settled = await pollJson(
    page.request,
    `/api/v1/access-grants/${encodeURIComponent(grantId)}`,
    (value) => ['revoked', 'denied', 'expired'].includes(value.state),
    'WORK_ACCESS_GRANT_REVOKE_STATUS_FAILED',
    120_000,
  )
  if (settled.state !== 'revoked') throw new Error(`WORK_ACCESS_GRANT_NOT_REVOKED:${settled.state}`)
  return settled
}

