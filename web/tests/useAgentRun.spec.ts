import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { ref } from 'vue'
import { useAgentRun } from '@/composables/useAgentRun'
import { createAgentRun, getAgentRun } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    createAgentRun: vi.fn(),
    getAgentRun: vi.fn(),
    cancelAgentRun: vi.fn(),
    retryAgentRunTrack: vi.fn(),
  }
})

function makeRun(state: 'running' | 'succeeded' | 'failed') {
  return {
    id: 'run-1',
    courseId: 'course-1',
    packageId: 'pkg-1',
    policyId: 'policy-1',
    policyRevision: 1,
    requestedRuntime: 'container' as const,
    revision: 1,
    state,
    tracks: [
      {
        kind: 'environment' as const,
        candidateId: null,
        attempts: [
          {
            number: 1,
            state,
            inputSha256: 'a'.repeat(64),
            outputSha256: state === 'succeeded' ? 'b'.repeat(64) : null,
            diagnosticCode: null,
            usage: { inputTokens: 0, outputTokens: 0, requests: 0, costMicrousd: 0 },
            usageObserved: state === 'succeeded',
          },
        ],
      },
      {
        kind: 'evaluation' as const,
        candidateId: null,
        attempts: [
          {
            number: 1,
            state,
            inputSha256: 'a'.repeat(64),
            outputSha256: state === 'succeeded' ? 'b'.repeat(64) : null,
            diagnosticCode: null,
            usage: { inputTokens: 0, outputTokens: 0, requests: 0, costMicrousd: 0 },
            usageObserved: state === 'succeeded',
          },
        ],
      },
    ],
  }
}

describe('useAgentRun', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    vi.useFakeTimers({ shouldAdvanceTime: true })
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('surfaces poll error, stops polling, and resumes after transient failure', async () => {
    const courseId = ref('course-1')
    const agent = useAgentRun(courseId)

    vi.mocked(createAgentRun).mockResolvedValue({
      data: makeRun('running'),
      error: undefined as never,
    })

    const responses = [
      { data: makeRun('running'), error: undefined as never },
      { data: makeRun('running'), error: undefined as never },
      {
        data: undefined as never,
        error: {
          response: {
            data: {
              diagnosticCode: 'AGENT_RUN_POLL_TRANSIENT',
              detail: 'transient failure',
              retryable: true,
            },
          },
        },
      },
      { data: makeRun('succeeded'), error: undefined as never },
    ]
    let callIndex = 0
    vi.mocked(getAgentRun).mockImplementation(async () => {
      const response = responses[callIndex]
      callIndex += 1
      return response
    })

    await agent.start({
      packageId: 'pkg-1',
      packageRevision: 1,
      packageSha256: 'a'.repeat(64),
      policyId: 'policy-1',
      policyRevision: 1,
      requestedRuntime: 'container',
    })

    expect(agent.run.kind).toBe('success')
    expect(agent.polling).toBe(true)

    // First scheduled poll keeps the run running.
    await vi.advanceTimersByTimeAsync(3000)
    expect(agent.polling).toBe(true)
    expect(agent.pollError).toBeNull()

    // Second scheduled poll fails transiently.
    await vi.advanceTimersByTimeAsync(3000)
    expect(agent.polling).toBe(false)
    expect(agent.pollError).not.toBeNull()
    expect(agent.pollError?.code).toBe('AGENT_RUN_POLL_TRANSIENT')

    // Resume polling.
    agent.resumePolling()
    expect(agent.polling).toBe(true)
    expect(agent.pollError).toBeNull()

    // Third poll returns the terminal succeeded state.
    await vi.advanceTimersByTimeAsync(3000)
    expect(agent.run.kind).toBe('success')
    if (agent.run.kind === 'success') {
      expect(agent.run.data.state).toBe('succeeded')
    }
    expect(agent.polling).toBe(false)
  })
})
