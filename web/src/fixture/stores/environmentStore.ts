import type {
  CreateEnvironmentRequestSchema,
  DesiredEnvironmentState,
  EnvironmentInstanceSchema,
  EnvironmentOperationAcceptedSchema,
  EnvironmentOperationKind,
  EnvironmentSummary,
  ObservedEnvironmentState,
} from '@/generated/contracts'
import { nowIso } from '../utils/clock'
import { nextStreamSequence, nextUuid7 } from '../utils/identity'
import { nextRevision } from '../utils/sequence'
import { appendEvent } from './eventLog'
import { listEndpoints, seedEndpointsFor, toEnvironmentEndpoints } from './endpointStore'
import { listTemplateReleases } from './templateReleaseStore'
import {
  createOperation,
  findOperation,
  toEnvironmentOperation,
  toOperationSnapshot,
  type StoredOperation,
} from './operationStore'
import type { FixtureActor } from './actorStore'

interface StoredEnvironment {
  instance: EnvironmentInstanceSchema
  displayLabel: string
  currentOperationId?: string
}

const environments = new Map<string, StoredEnvironment>()
const idempotencyMap = new Map<string, EnvironmentOperationAcceptedSchema>()

const RELEASE_ID = 'release-fixture-001'
const PROVIDER_BINDING = 'fixture-capacity-provider'

export function seedEnvironments(): string[] {
  environments.clear()
  idempotencyMap.clear()

  const ready = createEnvironmentInternal({
    courseId: 'course-101',
    ownerId: 'fixture-actor-teacher',
    displayLabel: 'Ready Experiment',
    class: 'experiment',
    runtimeKind: 'virtual_machine',
    desiredState: 'running',
    observedState: 'ready',
  })
  seedEndpointsFor(ready.instance.id, [
    { id: `ep-${ready.instance.id}-ssh`, protocol: 'ssh', health: 'healthy' },
    { id: `ep-${ready.instance.id}-https`, protocol: 'https', health: 'healthy' },
  ])
  ready.instance.endpoints = toEnvironmentEndpoints(ready.instance.id)

  const stopped = createEnvironmentInternal({
    courseId: 'course-101',
    ownerId: 'fixture-actor-teacher',
    displayLabel: 'Stopped Work',
    class: 'work',
    runtimeKind: 'container',
    desiredState: 'stopped',
    observedState: 'stopped',
  })
  seedEndpointsFor(stopped.instance.id, [
    { id: `ep-${stopped.instance.id}-ssh`, protocol: 'ssh', health: 'unhealthy' },
  ])
  stopped.instance.endpoints = toEnvironmentEndpoints(stopped.instance.id)

  const failed = createEnvironmentInternal({
    courseId: 'course-102',
    ownerId: 'fixture-actor-teacher',
    displayLabel: 'Failed Experiment',
    class: 'experiment',
    runtimeKind: 'virtual_machine',
    desiredState: 'running',
    observedState: 'failed',
    failedPhase: 'provisioning',
    lastDiagnosticCode: 'LW_PROVIDER_PROVISIONING_FAILED',
  })
  seedEndpointsFor(failed.instance.id, [
    { id: `ep-${failed.instance.id}-ssh`, protocol: 'ssh', health: 'unhealthy' },
  ])
  failed.instance.endpoints = toEnvironmentEndpoints(failed.instance.id)

  const deleting = createEnvironmentInternal({
    courseId: 'course-101',
    ownerId: 'fixture-actor-teacher',
    displayLabel: 'Deleting Work',
    class: 'work',
    runtimeKind: 'container',
    desiredState: 'deleted',
    observedState: 'deleting',
  })
  seedEndpointsFor(deleting.instance.id, [
    { id: `ep-${deleting.instance.id}-https`, protocol: 'https', health: 'removed' },
  ])
  deleting.instance.endpoints = toEnvironmentEndpoints(deleting.instance.id)

  const lifecycleFailure = createEnvironmentInternal({
    id: LIFECYCLE_FAILURE_ENV_ID,
    courseId: 'course-101',
    ownerId: 'fixture-actor-teacher',
    displayLabel: 'Lifecycle Failure Experiment',
    class: 'experiment',
    runtimeKind: 'virtual_machine',
    desiredState: 'running',
    observedState: 'failed',
    failedPhase: 'provisioning',
    lastDiagnosticCode: 'LW_PROVIDER_PROVISIONING_FAILED',
  })
  seedEndpointsFor(lifecycleFailure.instance.id, [
    { id: `ep-${lifecycleFailure.instance.id}-ssh`, protocol: 'ssh', health: 'unhealthy' },
  ])
  lifecycleFailure.instance.endpoints = toEnvironmentEndpoints(lifecycleFailure.instance.id)

  return [ready.instance.id, stopped.instance.id, failed.instance.id, deleting.instance.id, lifecycleFailure.instance.id]
}

interface CreateEnvironmentOptions {
  courseId: string
  ownerId: string
  displayLabel: string
  class: EnvironmentInstanceSchema['class']
  runtimeKind: EnvironmentInstanceSchema['runtimeKind']
  desiredState: DesiredEnvironmentState
  observedState: ObservedEnvironmentState
  failedPhase?: ObservedEnvironmentState | null
  lastDiagnosticCode?: string | null
  id?: string
}

export const LIFECYCLE_FAILURE_ENV_ID = 'env-lifecycle-failure'

function createEnvironmentInternal(options: CreateEnvironmentOptions): StoredEnvironment {
  const id = options.id ?? nextUuid7('env')
  const revision = nextRevision()
  const now = nowIso()
  const instance: EnvironmentInstanceSchema = {
    id,
    courseId: options.courseId,
    ownerId: options.ownerId,
    class: options.class,
    runtimeKind: options.runtimeKind,
    releaseId: RELEASE_ID,
    releaseVersion: 1,
    revision,
    generation: 1,
    observedGeneration: 1,
    desiredState: options.desiredState,
    observedState: options.observedState,
    failedPhase: options.failedPhase ?? null,
    lastDiagnosticCode: options.lastDiagnosticCode ?? null,
    eligibilityExpiresAt: now,
    endpoints: [],
    operation: createEnvironmentOperation(id),
    providerBinding: PROVIDER_BINDING,
  }
  const stored: StoredEnvironment = { instance, displayLabel: options.displayLabel }
  environments.set(id, stored)
  return stored
}

function createEnvironmentOperation(environmentId: string): EnvironmentInstanceSchema['operation'] {
  const op = createOperation(environmentId, 'create')
  return toEnvironmentOperation(op)
}

export function createEnvironment(
  request: CreateEnvironmentRequestSchema,
  actor: FixtureActor,
  idempotencyKey: string,
): EnvironmentOperationAcceptedSchema {
  const cached = idempotencyMap.get(idempotencyKey)
  if (cached) return cached

  // Resolve the runtime kind from the referenced release so created
  // environments expose the correct endpoints (ssh for VM, https for
  // container code-server).
  const release = listTemplateReleases(request.courseId).find(
    (r) => r.id === request.releaseId && r.version === request.releaseVersion,
  )
  const runtimeKind = release?.runtimeKind ?? 'virtual_machine'

  const stored = createEnvironmentInternal({
    courseId: request.courseId,
    ownerId: actor.actorId,
    displayLabel: request.displayLabel ?? 'Untitled Environment',
    class: 'experiment',
    runtimeKind,
    desiredState: 'running',
    observedState: 'requested',
  })
  if (runtimeKind === 'container') {
    seedEndpointsFor(stored.instance.id, [
      { id: `ep-${stored.instance.id}-https`, protocol: 'https' },
    ])
  } else {
    seedEndpointsFor(stored.instance.id, [
      { id: `ep-${stored.instance.id}-ssh`, protocol: 'ssh' },
    ])
  }
  stored.instance.endpoints = toEnvironmentEndpoints(stored.instance.id)

  const op = createOperation(stored.instance.id, 'create')
  stored.instance.operation = toEnvironmentOperation(op)
  stored.currentOperationId = op.operationId

  appendEnvironmentChanged(stored.instance)
  appendOperationChanged(stored.instance, op)

  const accepted: EnvironmentOperationAcceptedSchema = {
    environmentId: stored.instance.id,
    operationId: op.operationId,
    revision: stored.instance.revision,
    statusUrl: `/api/v1/environments/${stored.instance.id}/operations/${op.operationId}`,
  }

  transitionOperationToCompleted(stored, op, 'ready')

  idempotencyMap.set(idempotencyKey, accepted)
  return accepted
}

export function getEnvironment(environmentId: string): EnvironmentInstanceSchema | undefined {
  const stored = environments.get(environmentId)
  if (!stored) return undefined
  stored.instance.endpoints = toEnvironmentEndpoints(environmentId)
  return stored.instance
}

export function listEnvironments(courseId: string): EnvironmentSummary[] {
  return Array.from(environments.values())
    .filter((stored) => stored.instance.courseId === courseId)
    .map((stored) => toEnvironmentSummary(stored))
}

export function deleteEnvironment(
  environmentId: string,
  ifMatch: string,
  idempotencyKey: string,
): EnvironmentOperationAcceptedSchema | null {
  const cached = idempotencyMap.get(idempotencyKey)
  if (cached) return cached

  const stored = environments.get(environmentId)
  if (!stored) return null
  if (String(stored.instance.revision) !== ifMatch) return null

  stored.instance.desiredState = 'deleted'
  stored.instance.observedState = 'deleting'
  stored.instance.revision = nextRevision()
  const op = createOperation(environmentId, 'delete')
  stored.instance.operation = toEnvironmentOperation(op)
  stored.currentOperationId = op.operationId

  appendEnvironmentChanged(stored.instance)
  appendOperationChanged(stored.instance, op)

  const accepted: EnvironmentOperationAcceptedSchema = {
    environmentId,
    operationId: op.operationId,
    revision: stored.instance.revision,
    statusUrl: `/api/v1/environments/${environmentId}/operations/${op.operationId}`,
  }
  idempotencyMap.set(idempotencyKey, accepted)
  return accepted
}

function performTransition(
  environmentId: string,
  kind: EnvironmentOperationKind,
  targetState: ObservedEnvironmentState,
  desiredState: DesiredEnvironmentState,
): EnvironmentOperationAcceptedSchema | null {
  const stored = environments.get(environmentId)
  if (!stored) return null

  const op = createOperation(environmentId, kind)
  stored.instance.operation = toEnvironmentOperation(op)
  stored.currentOperationId = op.operationId
  stored.instance.desiredState = desiredState
  stored.instance.observedState = 'updating'
  stored.instance.revision = nextRevision()

  appendEnvironmentChanged(stored.instance)
  appendOperationChanged(stored.instance, op)

  const accepted: EnvironmentOperationAcceptedSchema = {
    environmentId,
    operationId: op.operationId,
    revision: stored.instance.revision,
    statusUrl: `/api/v1/environments/${environmentId}/operations/${op.operationId}`,
  }

  transitionOperationToCompleted(stored, op, targetState)
  return accepted
}

export function startEnvironment(environmentId: string): EnvironmentOperationAcceptedSchema | null {
  const stored = environments.get(environmentId)
  if (!stored || stored.instance.observedState === 'deleted' || stored.instance.observedState === 'deleting') return null
  return performTransition(environmentId, 'start', 'ready', 'running')
}

export function stopEnvironment(environmentId: string): EnvironmentOperationAcceptedSchema | null {
  const stored = environments.get(environmentId)
  if (!stored || stored.instance.observedState === 'deleted' || stored.instance.observedState === 'deleting') return null
  return performTransition(environmentId, 'stop', 'stopped', 'stopped')
}

export function restartEnvironment(environmentId: string): EnvironmentOperationAcceptedSchema | null {
  const stored = environments.get(environmentId)
  if (!stored || stored.instance.observedState === 'deleted' || stored.instance.observedState === 'deleting') return null
  return performTransition(environmentId, 'restart', 'ready', 'running')
}

export function resetEnvironment(environmentId: string): EnvironmentOperationAcceptedSchema | null {
  const stored = environments.get(environmentId)
  if (!stored || stored.instance.observedState === 'deleted' || stored.instance.observedState === 'deleting') return null
  return performTransition(environmentId, 'reset', 'ready', 'running')
}

export function recoverEnvironment(environmentId: string): EnvironmentOperationAcceptedSchema | null {
  const stored = environments.get(environmentId)
  if (!stored || stored.instance.observedState === 'deleted' || stored.instance.observedState === 'deleting') return null
  return performTransition(environmentId, 'recover', 'ready', 'running')
}

export function retryEnvironment(environmentId: string): EnvironmentOperationAcceptedSchema | null {
  const stored = environments.get(environmentId)
  if (!stored || stored.instance.observedState === 'deleted' || stored.instance.observedState === 'deleting') return null
  return performTransition(environmentId, 'retry', 'ready', 'running')
}

export function cancelEnvironment(environmentId: string): EnvironmentOperationAcceptedSchema | null {
  const stored = environments.get(environmentId)
  if (!stored) return null
  const opId = stored.currentOperationId
  if (!opId) return null
  const op = createOperation(environmentId, 'cancel')
  stored.instance.operation = toEnvironmentOperation(op)
  stored.currentOperationId = op.operationId
  stored.instance.revision = nextRevision()
  appendEnvironmentChanged(stored.instance)
  appendOperationChanged(stored.instance, op)
  return {
    environmentId,
    operationId: op.operationId,
    revision: stored.instance.revision,
    statusUrl: `/api/v1/environments/${environmentId}/operations/${op.operationId}`,
  }
}

export function freezeEnvironment(
  environmentId: string,
  ifMatch: string,
  idempotencyKey: string,
): EnvironmentOperationAcceptedSchema | null {
  const cached = idempotencyMap.get(idempotencyKey)
  if (cached) return cached

  const stored = environments.get(environmentId)
  if (!stored) return null
  if (String(stored.instance.revision) !== ifMatch) return null

  const op = createOperation(environmentId, 'freeze')
  stored.instance.operation = toEnvironmentOperation(op)
  stored.currentOperationId = op.operationId
  stored.instance.desiredState = 'stopped'
  stored.instance.observedState = 'updating'
  stored.instance.revision = nextRevision()

  appendEnvironmentChanged(stored.instance)
  appendOperationChanged(stored.instance, op)

  const accepted: EnvironmentOperationAcceptedSchema = {
    environmentId,
    operationId: op.operationId,
    revision: stored.instance.revision,
    statusUrl: `/api/v1/environments/${environmentId}/operations/${op.operationId}`,
  }
  idempotencyMap.set(idempotencyKey, accepted)
  transitionOperationToCompleted(stored, op, 'stopped')

  // Freeze evidence: the frozen submission is archived to the object store with
  // an immutable version and digest. Exposed as an additive optional field on
  // the instance view so the UI can render it without fabricating it.
  const instance = stored.instance as EnvironmentInstanceSchema & {
    freezeEvidence?: EnvironmentInstanceSchema['cleanupEvidence']
  }
  instance.freezeEvidence = {
    artifactId: nextUuid7('artifact'),
    mediaType: 'application/vnd.labweaver.submission+tar',
    objectVersion: `freeze-${op.operationId}`,
    sha256: 'f'.repeat(64),
    sizeBytes: 4096,
    storeBinding: 'fixture-store',
  }
  return accepted
}

function transitionOperationToCompleted(
  stored: StoredEnvironment,
  op: StoredOperation,
  targetState: ObservedEnvironmentState,
): void {
  stored.instance.observedState = targetState
  stored.instance.revision = nextRevision()
  stored.instance.operation = toEnvironmentOperation(op)
  stored.instance.operation.state = 'succeeded'
  op.publicStatus = 'succeeded'
  op.state = 'succeeded'
  op.terminalAt = nowIso()
  op.currentRevision = nextRevision()
  appendEnvironmentChanged(stored.instance)
  appendOperationChanged(stored.instance, op)
}

function toEnvironmentSummary(stored: StoredEnvironment): EnvironmentSummary {
  const instance = stored.instance
  const endpoints = listEndpoints(instance.id)
  const healthyCount = endpoints.filter((e) => e.health === 'healthy').length
  const activeGrantCount = 0
  const accessEligibility: EnvironmentSummary['access'] = {
    state: healthyCount > 0 ? 'eligible' : 'ineligible',
    healthyEndpointCount: healthyCount,
    activeGrantCount,
  }

  let currentOperation: EnvironmentSummary['currentOperation'] = null
  if (stored.currentOperationId) {
    // eslint-disable-next-line @typescript-eslint/no-use-before-define
    currentOperation = getCurrentOperationSnapshot(stored)
  }

  return {
    id: instance.id,
    courseId: instance.courseId,
    displayLabel: stored.displayLabel,
    class: instance.class,
    runtimeKind: instance.runtimeKind,
    releaseId: instance.releaseId,
    releaseVersion: instance.releaseVersion,
    revision: instance.revision,
    desiredState: instance.desiredState,
    observedState: instance.observedState,
    eligibilityExpiresAt: instance.eligibilityExpiresAt,
    createdAt: nowIso(),
    updatedAt: nowIso(),
    lastChangedStreamSequence: nextStreamSequence(),
    owner: { relation: 'managed' },
    access: accessEligibility,
    currentOperation,
  }
}

function getCurrentOperationSnapshot(stored: StoredEnvironment): EnvironmentSummary['currentOperation'] {
  if (!stored.currentOperationId) return null
  const op = findOperation(stored.currentOperationId)
  if (!op) return null
  return toOperationSnapshot(op)
}

function appendEnvironmentChanged(instance: EnvironmentInstanceSchema): void {
  appendEvent({
    courseId: instance.courseId,
    projectId: null,
    streamSequence: nextStreamSequence(),
    eventId: nextUuid7('evt'),
    effectiveAt: nowIso(),
    data: {
      kind: 'environment_changed',
      environmentId: instance.id,
      observedState: instance.observedState,
      operationId: instance.operation.id,
      revision: instance.revision,
    },
  })
}

function appendOperationChanged(instance: EnvironmentInstanceSchema, op: StoredOperation): void {
  appendEvent({
    courseId: instance.courseId,
    projectId: null,
    streamSequence: nextStreamSequence(),
    eventId: nextUuid7('evt'),
    effectiveAt: nowIso(),
    data: {
      kind: 'operation_changed',
      environmentId: instance.id,
      operationId: op.operationId,
      state: op.publicStatus,
      revision: op.currentRevision,
    },
  })
}

export function resetEnvironmentStore(): void {
  environments.clear()
  idempotencyMap.clear()
}
