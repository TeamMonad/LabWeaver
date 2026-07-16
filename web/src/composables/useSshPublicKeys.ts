import { reactive, ref } from 'vue'
import type { AxiosError } from 'axios'
import { listSshPublicKeys, createSshPublicKey, deleteSshPublicKey } from '@/generated/contracts'
import type { SshPublicKeySchema, ProblemDetails } from '@/generated/contracts'
import { makeDiagnostic, type AsyncState } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

export function useSshPublicKeys() {
  const keys = ref<AsyncState<SshPublicKeySchema[]>>({ kind: 'idle' })
  const creating = ref(false)
  const deleting = ref<Set<string>>(new Set())

  async function load() {
    keys.value = { kind: 'loading', message: '加载 SSH 公钥…' }
    const result = await listSshPublicKeys()
    if (result.error) {
      const problem = ((result.error as AxiosError).response?.data ?? (result.error as { error?: ProblemDetails }).error) as
        | ProblemDetails
        | undefined
      keys.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'SSH_KEY_LIST_FAILED',
          problem?.detail ?? '加载 SSH 公钥失败',
          problem?.retryable ?? true,
        ),
      }
      return
    }
    const items = result.data.items ?? []
    keys.value = items.length > 0 ? { kind: 'success', data: items } : { kind: 'empty' }
  }

  async function add(publicKeyOpenssh: string) {
    creating.value = true
    try {
      const result = await createSshPublicKey({
        headers: { 'Idempotency-Key': idempotencyKey() },
        body: { publicKeyOpenssh },
      })
      if (result.error) {
        const problem = ((result.error as AxiosError).response?.data ?? (result.error as { error?: ProblemDetails }).error) as
          | ProblemDetails
          | undefined
        return {
          ok: false,
          diagnostic: makeDiagnostic(
            problem?.diagnosticCode ?? 'SSH_KEY_CREATE_FAILED',
            problem?.detail ?? '添加 SSH 公钥失败',
            problem?.retryable ?? true,
          ),
        }
      }
      await load()
      return { ok: true }
    } finally {
      creating.value = false
    }
  }

  async function remove(key: SshPublicKeySchema) {
    deleting.value.add(key.id)
    try {
      const result = await deleteSshPublicKey({
        path: { keyId: key.id },
        headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(key.revision) }
      })
      if (result.error) {
        const problem = ((result.error as AxiosError).response?.data ?? (result.error as { error?: ProblemDetails }).error) as
          | ProblemDetails
          | undefined
        return {
          ok: false,
          diagnostic: makeDiagnostic(
            problem?.diagnosticCode ?? 'SSH_KEY_DELETE_FAILED',
            problem?.detail ?? '删除 SSH 公钥失败',
            problem?.retryable ?? true,
          ),
        }
      }
      await load()
      return { ok: true }
    } finally {
      deleting.value.delete(key.id)
    }
  }

  return reactive({ keys, creating, deleting, load, add, remove })
}
