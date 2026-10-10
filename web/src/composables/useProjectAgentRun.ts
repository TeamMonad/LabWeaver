import { onScopeDispose, reactive, ref, watch } from 'vue'
import {
  cancelProjectAgentRun,
  createProjectAgentRun,
  createProjectWorkConfigurationRun,
  getProjectAgentRun,
  retryProjectAgentRunTrack,
} from '@/generated/contracts'
import type {
  AgentRunSchema,
  CreateAgentRunRequestSchema,
  CreateWorkConfigurationRunRequestSchema,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'

function errorDiagnostic(error: unknown, code: string, message: string): DiagnosticViewModel {
  const problem = extractProblemDetails(error)
  return makeDiagnostic(problem?.diagnosticCode ?? code, problem?.detail ?? message, problem?.retryable ?? true)
}

const TERMINAL_STATES = new Set<AgentRunSchema['state']>(['succeeded', 'failed', 'partially_succeeded', 'cancelled'])
type StartResult =
  | { data: AgentRunSchema; error?: undefined }
  | { data?: undefined; error: unknown }

export type ProjectAgentRunPurposeExpectation =
  | { kind: 'authoring'; environmentClass?: 'experiment' | 'work' }
  | { kind: 'work_configuration'; environmentId?: string }

function purposeDiagnostic(
  data: AgentRunSchema,
  expectedPurpose: ProjectAgentRunPurposeExpectation | undefined,
): DiagnosticViewModel | undefined {
  if (!expectedPurpose) return undefined
  if (data.purpose.kind !== expectedPurpose.kind) {
    return makeDiagnostic(
      'PROJECT_RUN_PURPOSE_MISMATCH',
      expectedPurpose.kind === 'work_configuration'
        ? '当前任务不是 Work 配置任务，已停止恢复。请从当前操作重新打开。'
        : '当前任务用途与当前页面不符，已停止恢复。请从当前操作重新打开。',
      false,
    )
  }
  if (expectedPurpose.kind === 'authoring' && data.purpose.kind === 'authoring' && expectedPurpose.environmentClass && data.purpose.environmentClass !== expectedPurpose.environmentClass) {
    return makeDiagnostic('PROJECT_RUN_PURPOSE_MISMATCH', '当前任务不属于该模板类型，已停止恢复。请重新打开对应任务。', false)
  }
  if (expectedPurpose.kind === 'work_configuration' && data.purpose.kind === 'work_configuration' && expectedPurpose.environmentId && data.purpose.environmentId !== expectedPurpose.environmentId) {
    return makeDiagnostic('PROJECT_RUN_ENVIRONMENT_MISMATCH', '当前任务绑定了另一 Work 环境，已停止恢复。请从该环境重新打开。', false)
  }
  return undefined
}

/** Project-owned Agent run lifecycle used by authoring and Work configuration. */
export function useProjectAgentRun(
  projectId: ReturnType<typeof ref<string | null>>,
  expectedPurpose?: ProjectAgentRunPurposeExpectation,
) {
  const run = ref<AsyncState<AgentRunSchema>>({ kind: 'idle' })
  const acting = ref<string | null>(null)
  const outcome = ref<DiagnosticViewModel | null>(null)
  let pollTimer: ReturnType<typeof setTimeout> | null = null
  let loadGeneration = 0
  let startRequestFingerprint: string | null = null
  let startRequestKey: string | null = null
  let startRequestSucceeded = false

  function stopPolling() {
    if (pollTimer) {
      clearTimeout(pollTimer)
      pollTimer = null
    }
  }

  function schedulePoll(runData: AgentRunSchema) {
    stopPolling()
    if (TERMINAL_STATES.has(runData.state)) return
    if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return
    pollTimer = setTimeout(() => void load(runData.id, true), 3000)
  }

  async function load(runId: string, silent = false): Promise<boolean> {
    const id = projectId.value
    const generation = ++loadGeneration
    if (!id || !runId) {
      run.value = { kind: 'blocked', diagnostic: makeDiagnostic('PROJECT_RUN_ID_MISSING', '缺少项目或生成任务标识。', false) }
      return false
    }
    if (!silent) run.value = { kind: 'loading', message: '加载生成任务…' }
    const result = await getProjectAgentRun({ path: { projectId: id, runId } })
    if (generation !== loadGeneration || projectId.value !== id) return false
    if (result.error) {
      run.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'PROJECT_RUN_LOAD_FAILED', '加载生成任务失败') }
      stopPolling()
      return false
    }
    if (result.data.id !== runId || result.data.projectId !== id) {
      run.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          'PROJECT_RUN_STALE_CONTEXT',
          '生成任务返回的项目引用已变化，已停止恢复以避免显示过期任务。请从当前项目重新打开。',
          false,
        ),
      }
      stopPolling()
      return false
    }
    const purposeError = purposeDiagnostic(result.data, expectedPurpose)
    if (purposeError) {
      run.value = { kind: 'error', diagnostic: purposeError }
      stopPolling()
      return false
    }
    run.value = { kind: 'success', data: result.data }
    schedulePoll(result.data)
    return true
  }

  async function start(input: Omit<CreateAgentRunRequestSchema, 'projectId'>): Promise<boolean> {
    return startRequest('agent', input, async (id, key) => {
      const result = await createProjectAgentRun({
        path: { projectId: id },
        headers: { 'Idempotency-Key': key },
        body: { ...input, projectId: id },
      })
      if ('error' in result && result.error !== undefined) return { error: result.error }
      if (!result.data) return { error: new Error('Generation task response did not include data') }
      return { data: result.data }
    }, 'PROJECT_RUN_START_FAILED', '启动生成任务失败')
  }

  async function startWorkConfiguration(input: Omit<CreateWorkConfigurationRunRequestSchema, 'projectId'>): Promise<boolean> {
    return startRequest('work_configuration', input, async (id, key) => {
      const result = await createProjectWorkConfigurationRun({
        path: { projectId: id },
        headers: { 'Idempotency-Key': key },
        body: { ...input, projectId: id },
      })
      if ('error' in result && result.error !== undefined) return { error: result.error }
      if (!result.data) return { error: new Error('Work configuration generation task response did not include data') }
      return { data: result.data }
    }, 'PROJECT_WORK_RUN_START_FAILED', '启动 Work 配置生成任务失败')
  }

  async function startRequest(
    kind: 'agent' | 'work_configuration',
    input: object,
    request: (projectId: string, requestKey: string) => Promise<StartResult>,
    fallbackCode: string,
    fallbackMessage: string,
  ): Promise<boolean> {
    const id = projectId.value
    if (!id || acting.value) return false
    const body = { ...input, projectId: id }
    const fingerprint = JSON.stringify({ kind, body })
    if (fingerprint !== startRequestFingerprint || startRequestSucceeded || !startRequestKey) {
      startRequestFingerprint = fingerprint
      startRequestKey = idempotencyKey()
      startRequestSucceeded = false
    }
    const requestKey = startRequestKey
    const generation = loadGeneration
    acting.value = 'start'
    outcome.value = null
    stopPolling()
    try {
      const result = await request(id, requestKey)
      if (generation !== loadGeneration || projectId.value !== id) return false
      if (result.error) {
        outcome.value = errorDiagnostic(result.error, fallbackCode, fallbackMessage)
        return false
      }
      if (!result.data) {
        outcome.value = makeDiagnostic(fallbackCode, fallbackMessage, true)
        return false
      }
      const accepted = result.data
      const purposeError = purposeDiagnostic(accepted, expectedPurpose)
      if (purposeError) {
        run.value = { kind: 'error', diagnostic: purposeError }
        stopPolling()
        return false
      }
      startRequestSucceeded = true
      run.value = { kind: 'success', data: accepted }
      outcome.value = makeDiagnostic('PROJECT_RUN_ACCEPTED', '生成任务已受理。', false)
      schedulePoll(accepted)
      return true
    } finally {
      acting.value = null
    }
  }

  function reset() {
    loadGeneration += 1
    stopPolling()
    run.value = { kind: 'idle' }
    outcome.value = null
    startRequestFingerprint = null
    startRequestKey = null
    startRequestSucceeded = false
  }

  function invalidate(diagnostic: DiagnosticViewModel) {
    loadGeneration += 1
    stopPolling()
    run.value = { kind: 'error', diagnostic }
    outcome.value = diagnostic
    startRequestFingerprint = null
    startRequestKey = null
    startRequestSucceeded = false
  }

  watch(projectId, (id) => {
    loadGeneration += 1
    stopPolling()
    run.value = id ? { kind: 'idle' } : { kind: 'blocked', diagnostic: makeDiagnostic('PROJECT_CONTEXT_MISSING', '缺少项目上下文，无法读取生成任务。', false) }
    outcome.value = null
    startRequestFingerprint = null
    startRequestKey = null
    startRequestSucceeded = false
  })

  async function cancel(): Promise<boolean> {
    const id = projectId.value
    if (!id || run.value.kind !== 'success' || acting.value) return false
    const current = run.value.data
    const generation = loadGeneration
    acting.value = 'cancel'
    outcome.value = null
    try {
      const result = await cancelProjectAgentRun({
        path: { projectId: id, runId: current.id },
        headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(current.revision) },
      })
      if (generation !== loadGeneration || projectId.value !== id) return false
      if (result.error) {
        outcome.value = errorDiagnostic(result.error, 'PROJECT_RUN_CANCEL_FAILED', '取消生成任务失败')
        return false
      }
      run.value = { kind: 'success', data: result.data }
      outcome.value = makeDiagnostic('PROJECT_RUN_CANCEL_ACCEPTED', '取消请求已接受。', false)
      schedulePoll(result.data)
      return true
    } finally {
      acting.value = null
    }
  }

  async function retryTrack(track: 'environment' | 'evaluation' | 'work_configuration'): Promise<boolean> {
    const id = projectId.value
    if (!id || run.value.kind !== 'success' || acting.value) return false
    const current = run.value.data
    const generation = loadGeneration
    acting.value = `retry:${track}`
    outcome.value = null
    try {
      const result = await retryProjectAgentRunTrack({
        path: { projectId: id, runId: current.id, track },
        headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(current.revision) },
      })
      if (generation !== loadGeneration || projectId.value !== id) return false
      if (result.error) {
        outcome.value = errorDiagnostic(result.error, 'PROJECT_RUN_RETRY_FAILED', `重试 ${track} 轨道失败`)
        return false
      }
      run.value = { kind: 'success', data: result.data }
      outcome.value = makeDiagnostic('PROJECT_RUN_RETRY_ACCEPTED', `${track} 轨道重试已接受。`, false)
      schedulePoll(result.data)
      return true
    } finally {
      acting.value = null
    }
  }

  function onVisibilityChange() {
    if (run.value.kind === 'success') {
      if (document.visibilityState === 'visible') schedulePoll(run.value.data)
      else stopPolling()
    }
  }

  if (typeof document !== 'undefined') document.addEventListener('visibilitychange', onVisibilityChange)
  onScopeDispose(() => {
    stopPolling()
    if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', onVisibilityChange)
  })

  return reactive({ run, acting, outcome, load, start, startWorkConfiguration, cancel, retryTrack, stopPolling, reset, invalidate })
}
