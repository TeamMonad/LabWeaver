import { reactive, ref, type Ref } from 'vue'
import { issueConsoleCapability, listConsoleCapabilities } from '@/generated/contracts'
import type {
  ConsoleCapabilityAvailabilitySchema,
  ConsoleCapabilitySchema,
  ConsoleKind,
  IssueConsoleCapabilityRequestSchema,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

function errorDiagnostic(err: unknown, fallbackCode: string, fallbackDetail: string) {
  const problem = extractProblemDetails(err)
  return makeDiagnostic(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackDetail, problem?.retryable ?? true)
}

export function useConsoleCapability(grantId: Ref<string | undefined>) {
  const availability = ref<AsyncState<ConsoleCapabilityAvailabilitySchema>>({ kind: 'idle' })
  const issuing = ref(false)

  async function load() {
    const id = grantId.value
    if (!id) {
      availability.value = { kind: 'idle' }
      return
    }
    availability.value = { kind: 'loading', message: '加载控制台能力…' }
    const result = await listConsoleCapabilities({ path: { grantId: id } })
    if (result.error) {
      availability.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'CONSOLE_CAPABILITY_LIST_FAILED', '加载控制台能力失败') }
      return
    }
    availability.value = { kind: 'success', data: result.data }
  }

  async function issue(
    kind: ConsoleKind,
  ): Promise<{ ok: boolean; capability?: ConsoleCapabilitySchema; diagnostic?: ReturnType<typeof makeDiagnostic> }> {
    const id = grantId.value
    if (!id || availability.value.kind !== 'success') {
      return { ok: false, diagnostic: makeDiagnostic('CONSOLE_CAPABILITY_NOT_READY', '控制台能力未就绪。', false) }
    }
    issuing.value = true
    try {
      const grantRevision = availability.value.data.accessGrantRevision
      const environmentRevision = availability.value.data.environmentRevision
      const leaseFence: IssueConsoleCapabilityRequestSchema['expectedLeaseFence'] =
        availability.value.data.leaseFence ?? null
      const result = await issueConsoleCapability({
        path: { grantId: id },
        headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(grantRevision) },
        body: {
          kind,
          expectedAccessGrantRevision: grantRevision,
          expectedEnvironmentRevision: environmentRevision,
          expectedLeaseFence: leaseFence,
        },
      })
      if (result.error) {
        return { ok: false, diagnostic: errorDiagnostic(result.error, 'CONSOLE_CAPABILITY_ISSUE_FAILED', '签发控制台能力失败') }
      }
      return { ok: true, capability: result.data }
    } finally {
      issuing.value = false
    }
  }

  return reactive({ availability, issuing, load, issue })
}
