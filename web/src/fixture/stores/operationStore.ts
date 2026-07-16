import type {
  EnvironmentInstanceSchema,
  EnvironmentOperationSnapshotSchema,
  EnvironmentOperationStatus,
  OperationState,
} from '@/generated/contracts'
import { nowIso } from '../utils/clock'
import { nextRequestId, nextStreamSequence, nextUuid7 } from '../utils/identity'
import { nextRevision } from '../utils/sequence'

export interface StoredOperation {
  operationId: string
  environmentId: string
  kind: EnvironmentInstanceSchema['operation']['kind']
  state: OperationState
  publicStatus: EnvironmentOperationStatus
  acceptedAt: string
  updatedAt: string
  terminalAt?: string | null
  timedOutAt?: string | null
  acceptedRevision: number
  currentRevision: number
  attempt: number
  maxAttempts: number
  deadlineAt: string
  diagnosticCode?: string | null
  traceId: string
  requestId: string
  cancelEligible: boolean
  retryEligible: boolean
}

const operations = new Map<string, StoredOperation>()

export function createOperation(
  environmentId: string,
  kind: StoredOperation['kind'],
  options: {
    initialStatus?: EnvironmentOperationStatus
    diagnosticCode?: string
    timedOutAt?: string | null
  } = {},
): StoredOperation {
  const acceptedAt = nowIso()
  const op: StoredOperation = {
    operationId: nextUuid7('op'),
    environmentId,
    kind,
    state: 'accepted',
    publicStatus: options.initialStatus ?? 'accepted',
    acceptedAt,
    updatedAt: acceptedAt,
    terminalAt: options.initialStatus && isTerminal(options.initialStatus) ? acceptedAt : null,
    timedOutAt: options.timedOutAt ?? null,
    acceptedRevision: nextRevision(),
    currentRevision: nextRevision(),
    attempt: 1,
    maxAttempts: 3,
    deadlineAt: nowIso(),
    diagnosticCode: options.diagnosticCode ?? null,
    traceId: `trace-${nextRequestId()}`,
    requestId: nextRequestId(),
    cancelEligible: kind !== 'delete' && kind !== 'cleanup' && kind !== 'freeze',
    retryEligible: kind === 'retry' || kind === 'recover',
  }
  operations.set(op.operationId, op)
  return op
}

export function findOperation(operationId: string): StoredOperation | undefined {
  return operations.get(operationId)
}

export function findOperationsForEnvironment(environmentId: string): StoredOperation[] {
  return Array.from(operations.values()).filter((op) => op.environmentId === environmentId)
}

export function updateOperationStatus(
  operationId: string,
  status: EnvironmentOperationStatus,
  options: { diagnosticCode?: string; terminalAt?: string } = {},
): StoredOperation | undefined {
  const op = operations.get(operationId)
  if (!op) return undefined
  op.publicStatus = status
  op.updatedAt = nowIso()
  if (status === 'running') {
    op.state = 'running'
  } else if (status === 'cancelling') {
    op.state = 'cancelling'
  } else if (status === 'succeeded') {
    op.state = 'succeeded'
    op.terminalAt = options.terminalAt ?? nowIso()
  } else if (status === 'failed' || status === 'cancelled' || status === 'timed_out') {
    op.state = status === 'timed_out' ? 'failed' : (status as OperationState)
    op.terminalAt = options.terminalAt ?? nowIso()
    if (status === 'timed_out') {
      op.timedOutAt = nowIso()
    }
  }
  if (options.diagnosticCode) {
    op.diagnosticCode = options.diagnosticCode
  }
  op.currentRevision = nextRevision()
  return op
}

export function cancelOperation(operationId: string): StoredOperation | undefined {
  const op = operations.get(operationId)
  if (!op) return undefined
  if (!op.cancelEligible || isTerminal(op.publicStatus)) {
    return undefined
  }
  return updateOperationStatus(operationId, 'cancelled')
}

export function toOperationSnapshot(operation: StoredOperation): EnvironmentOperationSnapshotSchema {
  return {
    operationId: operation.operationId,
    environmentId: operation.environmentId,
    kind: operation.kind,
    state: operation.publicStatus,
    acceptedAt: operation.acceptedAt,
    acceptedRevision: operation.acceptedRevision,
    currentRevision: operation.currentRevision,
    updatedAt: operation.updatedAt,
    terminalAt: operation.terminalAt,
    timedOutAt: operation.timedOutAt,
    deadlineAt: operation.deadlineAt,
    attempt: operation.attempt,
    maxAttempts: operation.maxAttempts,
    traceId: operation.traceId,
    requestId: operation.requestId,
    cancelEligible: operation.cancelEligible,
    retryEligible: operation.retryEligible,
    lastChangedStreamSequence: nextStreamSequence(),
    diagnosticCode: operation.diagnosticCode,
  }
}

export function toEnvironmentOperation(operation: StoredOperation): EnvironmentInstanceSchema['operation'] {
  return {
    id: operation.operationId,
    environmentId: operation.environmentId,
    kind: operation.kind,
    state: operation.state,
    acceptedAt: operation.acceptedAt,
    updatedAt: operation.updatedAt,
    acceptedRevision: operation.acceptedRevision,
    currentRevision: operation.currentRevision,
    attempt: operation.attempt,
    maxAttempts: operation.maxAttempts,
    deadlineAt: operation.deadlineAt,
    actorId: 'fixture-actor-teacher',
    traceId: operation.traceId,
    nextAttemptAt: operation.deadlineAt,
    providerStep: 0,
    preserveMutableDisk: false,
    diagnosticCode: operation.diagnosticCode,
  }
}

function isTerminal(status: EnvironmentOperationStatus): boolean {
  return status === 'succeeded' || status === 'failed' || status === 'cancelled' || status === 'timed_out'
}

export function seedTimedOutOperation(environmentId: string): StoredOperation {
  return createOperation(environmentId, 'start', { initialStatus: 'timed_out', diagnosticCode: 'LW_OPERATION_TIMEOUT' })
}

export function resetOperationStore(): void {
  operations.clear()
}
