import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { nextTick, ref } from 'vue'
import { useProjectAgentRun } from '@/composables/useProjectAgentRun'
import { createProjectAgentRun, getProjectAgentRun } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    createProjectAgentRun: vi.fn(),
    getProjectAgentRun: vi.fn(),
    cancelProjectAgentRun: vi.fn(),
    retryProjectAgentRunTrack: vi.fn(),
  }
})

function makeRun(state: 'running' | 'succeeded' | 'failed') {
  return {
    id: 'run-1',
    projectId: 'project-1',
    courseId: 'course-1',
    packageId: 'pkg-1',
    policyId: 'policy-1',
    policyRevision: 1,
    purpose: { kind: 'authoring' as const, environmentClass: 'experiment' as const },
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
            checkpoint: null,
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
            checkpoint: null,
            diagnosticCode: null,
            usage: { inputTokens: 0, outputTokens: 0, requests: 0, costMicrousd: 0 },
            usageObserved: state === 'succeeded',
          },
        ],
      },
    ],
  }
}

describe('useProjectAgentRun', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    vi.useFakeTimers({ shouldAdvanceTime: true })
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('starts through the project API and surfaces a transient polling failure for manual recovery', async () => {
    const projectId = ref<string | null>('project-1')
    const agent = useProjectAgentRun(projectId)

    vi.mocked(createProjectAgentRun).mockResolvedValue({
      data: makeRun('running'),
      error: undefined as never,
    })

    const responses = [
      { data: makeRun('running'), error: undefined as never },
      {
        data: undefined as never,
        error: {
          response: {
            data: {
              diagnosticCode: 'PROJECT_RUN_POLL_TRANSIENT',
              detail: 'transient failure',
              retryable: true,
            },
          },
        },
      },
      { data: makeRun('succeeded'), error: undefined as never },
    ]
    let callIndex = 0
    vi.mocked(getProjectAgentRun).mockImplementation(async () => {
      const response = responses[callIndex]
      callIndex += 1
      return response
    })

    await expect(agent.start({
      packageId: 'pkg-1',
      packageRevision: 1,
      policyId: 'policy-1',
      policyRevision: 1,
      environmentClass: 'experiment',
    })).resolves.toBe(true)

    expect(agent.run.kind).toBe('success')
    expect(createProjectAgentRun).toHaveBeenCalledWith(expect.objectContaining({
      path: { projectId: 'project-1' },
      body: expect.objectContaining({ projectId: 'project-1' }),
    }))

    await vi.advanceTimersByTimeAsync(3000)
    expect(agent.run.kind).toBe('success')

    await vi.advanceTimersByTimeAsync(3000)
    expect(agent.run.kind).toBe('error')
    if (agent.run.kind === 'error') {
      expect(agent.run.diagnostic.code).toBe('PROJECT_RUN_POLL_TRANSIENT')
    }

    await agent.load('run-1')
    expect(agent.run.kind).toBe('success')
    if (agent.run.kind === 'success') expect(agent.run.data.state).toBe('succeeded')
  })

  it('reuses the start idempotency key for an unchanged failed request and rotates it after success', async () => {
    const projectId = ref<string | null>('project-1')
    const agent = useProjectAgentRun(projectId)
    const input = {
      packageId: 'pkg-1',
      packageRevision: 1,
      policyId: 'policy-1',
      policyRevision: 1,
      environmentClass: 'experiment' as const,
    }
    vi.mocked(createProjectAgentRun)
      .mockResolvedValueOnce({ data: undefined as never, error: { response: { data: { diagnosticCode: 'START_FAILED', detail: 'retry me', retryable: true } } } as never })
      .mockResolvedValueOnce({ data: makeRun('running'), error: undefined as never })
      .mockResolvedValueOnce({ data: makeRun('running'), error: undefined as never })

    await expect(agent.start(input)).resolves.toBe(false)
    await expect(agent.start(input)).resolves.toBe(true)
    await expect(agent.start(input)).resolves.toBe(true)

    const keys = vi.mocked(createProjectAgentRun).mock.calls.map((call) => call[0].headers?.['Idempotency-Key'])
    expect(keys[0]).toBe(keys[1])
    expect(keys[2]).not.toBe(keys[1])
  })

  it('rotates the start key when the request body changes', async () => {
    const projectId = ref<string | null>('project-1')
    const agent = useProjectAgentRun(projectId)
    vi.mocked(createProjectAgentRun).mockResolvedValue({ data: makeRun('running'), error: undefined as never })

    await agent.start({ packageId: 'pkg-1', packageRevision: 1, policyId: 'policy-1', policyRevision: 1, environmentClass: 'experiment' })
    await agent.start({ packageId: 'pkg-2', packageRevision: 1, policyId: 'policy-1', policyRevision: 1, environmentClass: 'experiment' })

    const keys = vi.mocked(createProjectAgentRun).mock.calls.map((call) => call[0].headers?.['Idempotency-Key'])
    expect(keys[1]).not.toBe(keys[0])
    expect(createProjectAgentRun).toHaveBeenLastCalledWith(expect.objectContaining({ body: expect.objectContaining({ packageId: 'pkg-2' }) }))
  })

  it('ignores a load response that belongs to a previous project', async () => {
    const projectId = ref<string | null>('project-1')
    const agent = useProjectAgentRun(projectId)
    let resolveLoad!: (value: unknown) => void
    vi.mocked(getProjectAgentRun).mockReturnValueOnce(new Promise((resolve) => { resolveLoad = resolve }) as never)

    const pending = agent.load('run-1')
    projectId.value = 'project-2'
    await nextTick()
    resolveLoad({ data: makeRun('succeeded'), error: undefined })
    await pending

    expect(agent.run.kind).toBe('idle')
  })
})
