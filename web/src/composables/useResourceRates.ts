import { onScopeDispose, reactive, ref } from 'vue'
import { createResourceRate, endResourceRate, listResourceRates } from '@/generated/contracts'
import type { CreateResourceRateRequestSchema, EndResourceRateRequestSchema, ResourceRateSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey } from '@/utils/format'

export interface ResourceRateMutationOutcome {
  kind: 'success' | 'error'
  diagnostic: DiagnosticViewModel
  operation: 'create' | 'end'
}

interface EndResourceRateIntent {
  rateId: string
  label: string
  input: EndResourceRateRequestSchema
  requestKey: string
  succeeded: boolean
  resultUnknown: boolean
}

function diagnostic(error: unknown, code: string, message: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? code, problem?.detail ?? message, problem?.retryable ?? true)
}

function resourceRateLabel(rate: ResourceRateSchema | undefined): string {
  if (!rate) return '原费率'
  if (rate.unit === 'gpu_unit_second') {
    const mode = ({ exclusive: '独占', container_time_slice: '容器时间片', vm_vgpu: 'VM vGPU' } as Record<string, string>)[rate.gpuMode ?? ''] ?? 'GPU 分配'
    return rate.gpuClass ? `GPU ${rate.gpuClass}（${mode}）` : `GPU 费率（${mode}）`
  }
  return ({
    cpu_millicore_second: 'CPU 费率',
    memory_byte_second: '内存费率',
    storage_byte_second: '存储费率',
  } as Record<string, string>)[rate.unit] ?? '资源费率'
}

/** Administrator-managed immutable Resource rate versions. */
export function useResourceRates() {
  const rates = ref<AsyncState<ResourceRateSchema[]>>({ kind: 'idle' })
  const acting = ref<'load' | 'create' | 'end' | null>(null)
  const outcome = ref<ResourceRateMutationOutcome | null>(null)
  let generation = 0
  let createRequestFingerprint: string | null = null
  let createRequestKey: string | null = null
  let createRequestSucceeded = false
  let lastCreateInput: CreateResourceRateRequestSchema | null = null
  let endRequestFingerprint: string | null = null
  let endIntent: EndResourceRateIntent | null = null
  let disposed = false

  async function fetchRates(currentGeneration: number): Promise<boolean> {
    try {
      const result = await listResourceRates()
      if (disposed || currentGeneration !== generation) return false
      if (result.error) {
        rates.value = { kind: 'error', diagnostic: diagnostic(result.error, 'RESOURCE_RATES_LOAD_FAILED', '加载资源费率失败。') }
        return false
      } else {
        rates.value = result.data.length > 0 ? { kind: 'success', data: result.data } : { kind: 'empty' }
        return true
      }
    } catch (error) {
      if (disposed || currentGeneration !== generation) return false
      rates.value = { kind: 'error', diagnostic: diagnostic(error, 'RESOURCE_RATES_LOAD_FAILED', '加载资源费率失败。') }
      return false
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
    lastCreateInput = structuredClone(input)
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
        outcome.value = { kind: 'error', operation: 'create', diagnostic: diagnostic(error, 'RESOURCE_RATE_CREATE_FAILED', '创建资源费率失败。') }
        return false
      }
      if (result.error) {
        outcome.value = { kind: 'error', operation: 'create', diagnostic: diagnostic(result.error, 'RESOURCE_RATE_CREATE_FAILED', '创建资源费率失败。') }
        return false
      }
      createRequestSucceeded = true
      const refreshed = await fetchRates(generation)
      if (!disposed) outcome.value = { kind: 'success', operation: 'create', diagnostic: makeDiagnostic('RESOURCE_RATE_CREATED', refreshed ? '资源费率已创建。' : '资源费率已创建，但列表刷新失败。请仅刷新列表，不要重复创建该版本。', !refreshed) }
      return true
    } finally {
      acting.value = null
    }
  }

  async function retryCreate(): Promise<boolean> {
    return lastCreateInput && !createRequestSucceeded ? create(lastCreateInput) : false
  }

  async function fetchRatesForEndIntent(): Promise<'ended' | 'open' | 'unknown'> {
    if (!endIntent || acting.value) return 'unknown'
    const currentGeneration = ++generation
    acting.value = 'load'
    try {
      const refreshed = await fetchRates(currentGeneration)
      if (!refreshed || disposed || currentGeneration !== generation) return 'unknown'
      if (rates.value.kind !== 'success') return 'unknown'
      const target = rates.value.data.find((rate) => rate.id === endIntent!.rateId)
      if (!target) return 'unknown'
      return target.effectiveUntil ? 'ended' : 'open'
    } finally {
      if (currentGeneration === generation) acting.value = null
    }
  }

  function currentRateLabel(rateId: string): string {
    if (rates.value.kind !== 'success') return '原费率'
    return resourceRateLabel(rates.value.data.find((rate) => rate.id === rateId))
  }

  async function submitEndIntent(intent: EndResourceRateIntent): Promise<boolean> {
    if (acting.value || intent.succeeded) return false
    const currentGeneration = generation
    acting.value = 'end'
    outcome.value = null
    try {
      let result
      try {
        result = await endResourceRate({
          path: { rateId: intent.rateId },
          headers: { 'Idempotency-Key': intent.requestKey },
          body: intent.input,
        })
      } catch (error) {
        const failure = diagnostic(error, 'RESOURCE_RATE_END_FAILED', '结束资源费率失败，结果可能尚未确认。')
        intent.resultUnknown = failure.retryable
        outcome.value = { kind: 'error', operation: 'end', diagnostic: failure }
        return false
      }
      if (result.error) {
        const failure = diagnostic(result.error, 'RESOURCE_RATE_END_FAILED', '结束资源费率失败，结果可能尚未确认。')
        intent.resultUnknown = failure.retryable
        outcome.value = { kind: 'error', operation: 'end', diagnostic: failure }
        return false
      }
      intent.succeeded = true
      intent.resultUnknown = false
      const refreshed = await fetchRates(currentGeneration)
      if (!disposed) {
        outcome.value = {
          kind: 'success',
          operation: 'end',
          diagnostic: makeDiagnostic(
            'RESOURCE_RATE_ENDED',
            refreshed
              ? '资源费率已安排结束。截止后不再用于新的用量核算；历史账单快照保留，现有环境不会被强制停止。'
              : '资源费率结束请求已接受，但列表刷新失败。请仅刷新列表，不要重复结束该费率。',
            !refreshed,
          ),
        }
      }
      return true
    } finally {
      if (currentGeneration === generation) acting.value = null
    }
  }

  async function end(rateId: string, input: EndResourceRateRequestSchema): Promise<boolean> {
    if (acting.value) return false
    if (endIntent && (endIntent.resultUnknown || endIntent.succeeded)) {
      const status = await fetchRatesForEndIntent()
      if (status === 'ended') {
        endIntent.succeeded = true
        endIntent.resultUnknown = false
        if (endIntent.rateId === rateId) {
          outcome.value = { kind: 'success', operation: 'end', diagnostic: makeDiagnostic('RESOURCE_RATE_END_CONFIRMED', `已确认截止时间：${endIntent.label}，费率已安排结束，请以列表为准。`) }
          return false
        }
        endIntent = null
        endRequestFingerprint = null
      } else if (status === 'unknown') {
        outcome.value = { kind: 'error', operation: 'end', diagnostic: makeDiagnostic('RESOURCE_RATE_END_UNCONFIRMED', `无法确认费率是否已结束（${endIntent.label}），请仅查询费率列表后重试；确认前不能操作另一费率。`, true) }
        return false
      } else {
        const message = endIntent.succeeded
          ? `已接受${endIntent.label}的结束请求，但列表尚未反映；请先仅刷新确认，不能重复提交。`
          : endIntent.rateId === rateId
            ? `${endIntent.label}仍在现行状态，请使用原结束请求的重试操作；修改截止时间不会直接再次提交。`
            : `${endIntent.label}仍在现行状态，请先使用原结束请求的重试操作；确认原请求后再操作其他费率。`
        outcome.value = { kind: 'error', operation: 'end', diagnostic: makeDiagnostic('RESOURCE_RATE_END_RETRY_REQUIRED', message, true) }
        return false
      }
    }
    const fingerprint = JSON.stringify({ rateId, input })
    if (fingerprint !== endRequestFingerprint || !endIntent) {
      endRequestFingerprint = fingerprint
      endIntent = { rateId, label: currentRateLabel(rateId), input: structuredClone(input), requestKey: idempotencyKey(), succeeded: false, resultUnknown: false }
    }
    if (endIntent.succeeded) return false
    return submitEndIntent(endIntent)
  }

  async function retryEnd(): Promise<boolean> {
    if (!endIntent || acting.value) return false
    const status = await fetchRatesForEndIntent()
    if (status === 'ended') {
      endIntent.succeeded = true
      endIntent.resultUnknown = false
      outcome.value = { kind: 'success', operation: 'end', diagnostic: makeDiagnostic('RESOURCE_RATE_END_CONFIRMED', `已确认截止时间：${endIntent.label}，费率已安排结束，请以列表为准。`) }
      return true
    }
    if (status === 'unknown') {
      outcome.value = { kind: 'error', operation: 'end', diagnostic: makeDiagnostic('RESOURCE_RATE_END_UNCONFIRMED', `无法确认费率是否已结束（${endIntent.label}），请仅查询费率列表后重试。`, true) }
      return false
    }
    if (endIntent.succeeded) {
      outcome.value = { kind: 'error', operation: 'end', diagnostic: makeDiagnostic('RESOURCE_RATE_END_UNCONFIRMED', `结束请求已接受，但${endIntent.label}的费率列表尚未反映，请稍后仅刷新列表确认。`, true) }
      return false
    }
    return submitEndIntent(endIntent)
  }

  onScopeDispose(() => {
    disposed = true
    generation += 1
  })

  return reactive({ rates, acting, outcome, load, create, retryCreate, end, retryEnd })
}
