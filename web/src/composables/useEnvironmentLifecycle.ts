import { reactive, ref, type Ref } from 'vue'
import {
  createEnvironment,
  startEnvironment,
  stopEnvironment,
  restartEnvironment,
  deleteEnvironment,
} from '@/generated/contracts'
import type { CreateEnvironmentRequestSchema, EnvironmentOperationAcceptedSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'
import { EXPERIMENT_CREATE_UNSUPPORTED } from '@/utils/runtimePermissions'

export type LifecycleAction = 'start' | 'stop' | 'restart' | 'delete'

/**
 * Create requires the original Resource request whose approval selects the
 * Environment target. Callers must not invent IDs; Experiment create without a
 * real handoff is surfaced as LW_RUNTIME_ALLOCATION_PATH_UNSUPPORTED.
 */
export type CreateEnvironmentInput = Pick<
  CreateEnvironmentRequestSchema,
  'releaseId' | 'releaseVersion' | 'resourceRequestId'
> &
  Partial<Pick<CreateEnvironmentRequestSchema, 'courseId' | 'displayLabel'>>

export type LifecycleMutationResult =
  | { ok: true; accepted: EnvironmentOperationAcceptedSchema }
  | { ok: false; diagnostic: DiagnosticViewModel }

export function useEnvironmentLifecycle(
  projectId: Ref<string | undefined>,
  courseId: Ref<string | undefined> = ref(undefined),
) {
  const operating = ref<Set<string>>(new Set())
  const lastAccepted = ref<EnvironmentOperationAcceptedSchema | null>(null)

  function track(id: string, active: boolean) {
    if (active) operating.value.add(id)
    else operating.value.delete(id)
  }

  /**
   * `intentKey` lets the caller pin one Idempotency-Key to one logical create
   * intent so that retrying after a network timeout replays the same create
   * instead of minting a second environment. Omit it only for first attempts.
   */
  async function create(request: CreateEnvironmentInput, intentKey?: string): Promise<LifecycleMutationResult> {
    const id = projectId.value
    if (!id) {
      return {
        ok: false,
        diagnostic: makeDiagnostic('PROJECT_CONTEXT_MISSING', '项目上下文缺失，无法创建环境。', false),
      }
    }
    const resourceRequestId = request.resourceRequestId?.trim()
    if (!resourceRequestId) {
      return {
        ok: false,
        diagnostic: makeDiagnostic(
          EXPERIMENT_CREATE_UNSUPPORTED.code,
          EXPERIMENT_CREATE_UNSUPPORTED.message,
          EXPERIMENT_CREATE_UNSUPPORTED.retryable,
        ),
      }
    }
    let result
    try {
      result = await createEnvironment({
        headers: { 'Idempotency-Key': intentKey ?? idempotencyKey() },
        body: {
          releaseId: request.releaseId,
          releaseVersion: request.releaseVersion,
          resourceRequestId,
          projectId: id,
          ...(request.displayLabel !== undefined ? { displayLabel: request.displayLabel } : {}),
          ...(request.courseId !== undefined
            ? { courseId: request.courseId }
            : courseId.value
              ? { courseId: courseId.value }
              : {}),
        },
      })
    } catch (error) {
      const problem = extractProblemDetails(error)
      return {
        ok: false,
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'ENVIRONMENT_CREATE_FAILED',
          problem?.detail ?? '创建环境失败，结果可能尚未确认。',
          problem?.retryable ?? true,
        ),
      }
    }
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

  async function act(environmentId: string, revision: number, action: LifecycleAction): Promise<LifecycleMutationResult> {
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
