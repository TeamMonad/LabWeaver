import { reactive, ref, type Ref } from 'vue'
import type { AxiosError } from 'axios'
import { apiClient } from '@/api/client'
import {
  createEnvironment,
  startEnvironment,
  stopEnvironment,
  restartEnvironment,
  deleteEnvironment,
} from '@/generated/contracts'
import type { CreateEnvironmentRequestSchema, OperationAccepted, ProblemDetails } from '@/generated/contracts'
import { makeDiagnostic } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

export type LifecycleAction = 'start' | 'stop' | 'restart' | 'delete'

export function useEnvironmentLifecycle(courseId: Ref<string | undefined>) {
  const operating = ref<Set<string>>(new Set())
  const lastAccepted = ref<OperationAccepted | null>(null)

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
      client: apiClient,
      headers: { 'Idempotency-Key': idempotencyKey() },
      body: request,
    })
    if (result.error) {
      const problem = ((result.error as AxiosError).response?.data ?? (result.error as { error?: ProblemDetails }).error) as
        | ProblemDetails
        | undefined
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
    // The contract currently returns OperationAccepted without environmentId.
    // Callers must obtain the environmentId from a list/query endpoint or query param.
    return { ok: true, accepted: result.data }
  }

  async function act(environmentId: string, revision: number, action: LifecycleAction) {
    track(`${environmentId}:${action}`, true)
    try {
      const headers = { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(revision) }
      const path = { environmentId }
      const fn =
        action === 'start'
          ? startEnvironment({ client: apiClient, path, headers })
          : action === 'stop'
            ? stopEnvironment({ client: apiClient, path, headers })
            : action === 'restart'
              ? restartEnvironment({ client: apiClient, path, headers })
              : deleteEnvironment({ client: apiClient, path, headers })
      const result = await fn
      if (result.error) {
        const problem = ((result.error as AxiosError).response?.data ?? (result.error as { error?: ProblemDetails }).error) as
          | ProblemDetails
          | undefined
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
