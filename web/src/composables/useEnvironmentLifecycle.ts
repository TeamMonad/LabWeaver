import { reactive, ref, type Ref } from 'vue'
import {
  createEnvironment,
  startEnvironment,
  stopEnvironment,
  restartEnvironment,
  deleteEnvironment,
} from '@/generated/contracts'
import type { CreateEnvironmentRequestSchema, EnvironmentOperationAcceptedSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

export type LifecycleAction = 'start' | 'stop' | 'restart' | 'delete'

export function useEnvironmentLifecycle(courseId: Ref<string | undefined>) {
  const operating = ref<Set<string>>(new Set())
  const lastAccepted = ref<EnvironmentOperationAcceptedSchema | null>(null)

  function track(id: string, active: boolean) {
    if (active) operating.value.add(id)
    else operating.value.delete(id)
  }

  async function create(request: CreateEnvironmentRequestSchema) {
    const id = courseId.value
    if (!id) {
      return {
        ok: false,
        diagnostic: makeDiagnostic('COURSE_CONTEXT_MISSING', '课程上下文缺失，无法创建环境。', false),
      }
    }
    const result = await createEnvironment({
      headers: { 'Idempotency-Key': idempotencyKey() },
      body: request,
    })
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      return {
        ok: false,
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'ENVIRONMENT_CREATE_FAILED',
          problem?.detail ?? '创建环境失败',
          problem?.retryable ?? true,
        ),
      }
    }
    lastAccepted.value = result.data
    return { ok: true, accepted: result.data }
  }

  async function act(environmentId: string, revision: number, action: LifecycleAction) {
    track(`${environmentId}:${action}`, true)
    try {
      const headers = { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(revision) }
      const path = { environmentId }
      const fn =
        action === 'start'
          ? startEnvironment({ path, headers })
          : action === 'stop'
            ? stopEnvironment({ path, headers })
            : action === 'restart'
              ? restartEnvironment({ path, headers })
              : deleteEnvironment({ path, headers })
      const result = await fn
      if (result.error) {
        const problem = extractProblemDetails(result.error)
        return {
          ok: false,
          diagnostic: makeDiagnostic(
            problem?.diagnosticCode ?? `ENVIRONMENT_${action.toUpperCase()}_FAILED`,
            problem?.detail ?? `${action} 失败`,
            problem?.retryable ?? true,
          ),
        }
      }
      lastAccepted.value = result.data
      return { ok: true, accepted: result.data }
    } finally {
      track(`${environmentId}:${action}`, false)
    }
  }

  return reactive({ operating, lastAccepted, create, act })
}
