import { onScopeDispose, reactive, ref } from 'vue'
import { createResourceRate, listResourceRates } from '@/generated/contracts'
import type { CreateResourceRateRequestSchema, ResourceRateSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey } from '@/utils/format'

export interface ResourceRateMutationOutcome {
  kind: 'success' | 'error'
  diagnostic: DiagnosticViewModel
}

function diagnostic(error: unknown, code: string, message: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? code, problem?.detail ?? message, problem?.retryable ?? true)
}

/** Administrator-managed immutable Resource rate versions. */
export function useResourceRates() {
  const rates = ref<AsyncState<ResourceRateSchema[]>>({ kind: 'idle' })
  const acting = ref<'load' | 'create' | null>(null)
  const outcome = ref<ResourceRateMutationOutcome | null>(null)
  let generation = 0
  let createRequestFingerprint: string | null = null
  let createRequestKey: string | null = null
  let createRequestSucceeded = false
  let disposed = false

  async function fetchRates(currentGeneration: number): Promise<void> {
    try {
      const result = await listResourceRates()
      if (disposed || currentGeneration !== generation) return
      if (result.error) {
        rates.value = { kind: 'error', diagnostic: diagnostic(result.error, 'RESOURCE_RATES_LOAD_FAILED', '加载资源费率失败。') }
      } else {
        rates.value = result.data.length > 0 ? { kind: 'success', data: result.data } : { kind: 'empty' }
      }
    } catch (error) {
      if (disposed || currentGeneration !== generation) return
      rates.value = { kind: 'error', diagnostic: diagnostic(error, 'RESOURCE_RATES_LOAD_FAILED', '加载资源费率失败。') }
    }
  }

  async function load(): Promise<void> {
    if (acting.value) return
    const currentGeneration = ++generation
    acting.value = 'load'
    outcome.value = null
    try {
      await fetchRates(currentGeneration)
    } finally {
      if (currentGeneration === generation) acting.value = null
    }
  }

  async function create(input: CreateResourceRateRequestSchema): Promise<boolean> {
    if (acting.value) return false
    const fingerprint = JSON.stringify(input)
    if (fingerprint !== createRequestFingerprint || createRequestSucceeded || !createRequestKey) {
      createRequestFingerprint = fingerprint
      createRequestKey = idempotencyKey()
      createRequestSucceeded = false
    }
    const requestKey = createRequestKey
    acting.value = 'create'
    outcome.value = null
    try {
      let result
      try {
        result = await createResourceRate({
          headers: { 'Idempotency-Key': requestKey },
          body: input,
        })
      } catch (error) {
        outcome.value = { kind: 'error', diagnostic: diagnostic(error, 'RESOURCE_RATE_CREATE_FAILED', '创建资源费率失败。') }
        return false
      }
      if (result.error) {
        outcome.value = { kind: 'error', diagnostic: diagnostic(result.error, 'RESOURCE_RATE_CREATE_FAILED', '创建资源费率失败。') }
        return false
      }
      createRequestSucceeded = true
      const success = { kind: 'success' as const, diagnostic: makeDiagnostic('RESOURCE_RATE_CREATED', '资源费率已创建。', false) }
      await fetchRates(generation)
      if (!disposed) outcome.value = success
      return true
    } finally {
      acting.value = null
    }
  }

  onScopeDispose(() => {
    disposed = true
    generation += 1
  })

  return reactive({ rates, acting, outcome, load, create })
}
