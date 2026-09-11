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

/** Project-owned Agent run lifecycle used by authoring and Work configuration. */
export function useProjectAgentRun(projectId: ReturnType<typeof ref<string | null>>) {
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

  async function load(runId: string, silent = false) {
    const id = projectId.value
    const generation = ++loadGeneration
    if (!id || !runId) {
      run.value = { kind: 'blocked', diagnostic: makeDiagnostic('PROJECT_RUN_ID_MISSING', '缺少项目或 AgentRun ID。', false) }
      return
    }
    if (!silent) run.value = { kind: 'loading', message: '加载 AgentRun…' }
    const result = await getProjectAgentRun({ path: { projectId: id, runId } })
    if (generation !== loadGeneration || projectId.value !== id) return
    if (result.error) {
      run.value = { kind: 'error', diagnostic: errorDiagnostic(result.error, 'PROJECT_RUN_LOAD_FAILED', '加载 AgentRun 失败') }
      stopPolling()
      return
    }
    run.value = { kind: 'success', data: result.data }
    schedulePoll(result.data)
  }

  async function start(input: Omit<CreateAgentRunRequestSchema, 'projectId'>): Promise<boolean> {
    return startRequest('agent', input, async (id, key) => {
      const result = await createProjectAgentRun({
        path: { projectId: id },
        headers: { 'Idempotency-Key': key },
        body: { ...input, projectId: id },
      })
      if ('error' in result && result.error !== undefined) return { error: result.error }
      if (!result.data) return { error: new Error('AgentRun response did not include data') }
      return { data: result.data }
    }, 'PROJECT_RUN_START_FAILED', '启动 AgentRun 失败')
  }

  async function startWorkConfiguration(input: Omit<CreateWorkConfigurationRunRequestSchema, 'projectId'>): Promise<boolean> {
    return startRequest('work_configuration', input, async (id, key) => {
      const result = await createProjectWorkConfigurationRun({
        path: { projectId: id },
        headers: { 'Idempotency-Key': key },
        body: { ...input, projectId: id },
      })
      if ('error' in result && result.error !== undefined) return { error: result.error }
      if (!result.data) return { error: new Error('Work configuration AgentRun response did not include data') }
      return { data: result.data }
    }, 'PROJECT_WORK_RUN_START_FAILED', '启动 Work 配置 AgentRun 失败')
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
      startRequestSucceeded = true
      run.value = { kind: 'success', data: accepted }
      outcome.value = makeDiagnostic('PROJECT_RUN_ACCEPTED', `AgentRun ${accepted.id} 已接受。`, false)
      schedulePoll(accepted)
      return true
    } finally {
      acting.value = null
    }
  }

  watch(projectId, (id) => {
    loadGeneration += 1
    stopPolling()
    run.value = id ? { kind: 'idle' } : { kind: 'blocked', diagnostic: makeDiagnostic('PROJECT_CONTEXT_MISSING', 'ȱ����Ŀ�� AgentRun��', false) }
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
        outcome.value = errorDiagnostic(result.error, 'PROJECT_RUN_CANCEL_FAILED', '取消 AgentRun 失败')
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

  return reactive({ run, acting, outcome, load, start, startWorkConfiguration, cancel, retryTrack, stopPolling })
}
