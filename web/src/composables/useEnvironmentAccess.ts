import { reactive, ref, type Ref } from 'vue'
import { listEnvironmentEndpoints, createAccessGrant, getAccessGrant, revokeAccessGrant } from '@/generated/contracts'
import type { AccessGrantSchema, EnvironmentEndpointSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

function addHours(date: Date, hours: number): string {
  const d = new Date(date.getTime() + hours * 60 * 60 * 1000)
  return d.toISOString()
}

const ACTIVATION_TIMEOUT_MS = 30_000
const ACTIVATION_POLL_MS = 500

export function useEnvironmentAccess(
  environmentId: Ref<string | undefined>,
  environmentRevision: Ref<number | undefined>,
  courseId: Ref<string | undefined>,
) {
  const endpoints = ref<AsyncState<EnvironmentEndpointSchema[]>>({ kind: 'idle' })
  const grant = ref<AsyncState<AccessGrantSchema>>({ kind: 'idle' })
  const creating = ref(false)

  async function loadEndpoints() {
    const id = environmentId.value
    if (!id) {
      endpoints.value = { kind: 'idle' }
      return
    }
    endpoints.value = { kind: 'loading', message: '加载环境入口…' }
    const result = await listEnvironmentEndpoints({ path: { environmentId: id } })
    if (result.error || !result.data) {
      const problem = extractProblemDetails(result.error)
      endpoints.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? (result.error ? 'ENDPOINT_LIST_FAILED' : 'ENDPOINT_LIST_RESPONSE_INVALID'),
          problem?.detail ?? (result.error ? '加载环境入口失败' : '环境入口响应缺少数据。'),
          problem?.retryable ?? Boolean(result.error),
        ),
      }
      return
    }
    const items = result.data.items ?? []
    endpoints.value = items.length > 0 ? { kind: 'success', data: items } : { kind: 'empty' }
  }

  async function createGrant() {
    const id = environmentId.value
    const rev = environmentRevision.value
    const cid = courseId.value
    const eps = endpoints.value.kind === 'success' ? endpoints.value.data : []
    if (!id || rev === undefined || !cid) {
      return {
        ok: false,
        diagnostic: makeDiagnostic('ACCESS_GRANT_NOT_READY', '环境信息缺失，无法签发访问授权。', false),
      }
    }
    if (eps.length === 0) {
      return {
        ok: false,
        diagnostic: makeDiagnostic('ACCESS_GRANT_NO_ENDPOINT', '没有可用的环境入口，无法签发访问授权。', false),
      }
    }
    creating.value = true
    try {
      const result = await createAccessGrant({
        path: { environmentId: id },
        headers: { 'Idempotency-Key': idempotencyKey() },
        body: {
          courseId: cid,
          environmentId: id,
          environmentRevision: rev,
          endpointIds: eps.map((e) => e.id),
          expiresAt: addHours(new Date(), 1),
        },
      })
      if (result.error) {
        const problem = extractProblemDetails(result.error)
        grant.value = {
          kind: 'error',
          diagnostic: makeDiagnostic(
            problem?.diagnosticCode ?? 'ACCESS_GRANT_CREATE_FAILED',
            problem?.detail ?? '签发访问授权失败',
            problem?.retryable ?? true,
          ),
        }
        return { ok: false }
      }
      grant.value = { kind: 'success', data: result.data }
      return await waitForActivation(result.data)
    } finally {
      creating.value = false
    }
  }

  async function waitForActivation(initial: AccessGrantSchema) {
    let current = initial
    const deadline = Date.now() + ACTIVATION_TIMEOUT_MS
    while (current.state === 'requested' && Date.now() < deadline) {
      await new Promise<void>((resolve) => window.setTimeout(resolve, ACTIVATION_POLL_MS))
      const result = await getAccessGrant({ path: { grantId: current.id } })
      if (result.error) {
        const problem = extractProblemDetails(result.error)
        const diagnostic = makeDiagnostic(
          problem?.diagnosticCode ?? 'ACCESS_GRANT_ACTIVATION_FAILED',
          problem?.detail ?? '访问授权激活状态读取失败',
          problem?.retryable ?? true,
        )
        grant.value = { kind: 'error', diagnostic }
        return { ok: false, diagnostic }
      }
      current = result.data
      grant.value = { kind: 'success', data: current }
    }
    if (current.state === 'active') return { ok: true }
    const diagnostic = makeDiagnostic(
      current.state === 'requested' ? 'ACCESS_GRANT_ACTIVATION_TIMEOUT' : 'ACCESS_GRANT_ACTIVATION_DENIED',
      current.state === 'requested' ? '访问授权激活超时，请稍后重试。' : `访问授权未激活：${current.state}`,
      current.state === 'requested',
    )
    grant.value = { kind: 'error', diagnostic }
    return { ok: false, diagnostic }
  }

  async function revokeGrant() {
    const current = grant.value.kind === 'success' ? grant.value.data : undefined
    if (!current) return { ok: false }
    const result = await revokeAccessGrant({
      path: { grantId: current.id },
      headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(current.revision) },
      body: { grantId: current.id, reasonCode: 'user_revoked' },
    })
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      return {
        ok: false,
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'ACCESS_GRANT_REVOKE_FAILED',
          problem?.detail ?? '撤销访问授权失败',
          problem?.retryable ?? true,
        ),
      }
    }
    grant.value = {
      kind: 'revoked',
      diagnostic: makeDiagnostic('ACCESS_GRANT_REVOKED', '访问授权已撤销。', false),
    }
    return { ok: true }
  }

  function resetGrant() {
    grant.value = { kind: 'idle' }
  }

  return reactive({ endpoints, grant, creating, loadEndpoints, createGrant, revokeGrant, resetGrant })
}
