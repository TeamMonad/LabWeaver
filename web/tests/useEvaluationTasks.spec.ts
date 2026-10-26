import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { effectScope, ref, type EffectScope } from 'vue'
import { getOwnProjectEvaluationTaskForSubmission, getOwnProjectEvaluationTaskProgress } from '@/generated/contracts'
import { useRunTask, useSubmissionTasks } from '@/composables/useEvaluationTasks'

vi.mock('@/generated/contracts', () => ({ getOwnProjectEvaluationTaskForSubmission: vi.fn(), getOwnProjectEvaluationTaskProgress: vi.fn() }))

const page = (nextCursor: unknown, attempt: number) => ({ data: {
  projectId: 'project', runId: 'run', revision: 5, state: 'running', nextCursor,
  current: [{ stepRunId: 'step', progress: { attempt: 5, phase: 'preparing', nextSteps: [] } }],
  history: [{ stepRunId: 'step', progress: { attempt, phase: 'failed', nextSteps: [] } }],
}, error: undefined }) as never

let scope: EffectScope
beforeEach(() => { vi.resetAllMocks(); scope = effectScope() })
afterEach(() => { scope.stop() })

describe('Evaluation history consumers', () => {
  it('reads all run history pages before publishing the current progress', async () => {
    vi.mocked(getOwnProjectEvaluationTaskProgress).mockResolvedValueOnce(page('older', 4)).mockResolvedValueOnce(page(null, 3))
    const task = scope.run(() => useRunTask(ref('project'), ref('run')))!
    await task.refresh()
    expect(getOwnProjectEvaluationTaskProgress).toHaveBeenNthCalledWith(2, { path: { projectId: 'project', runId: 'run' }, query: { limit: 100, cursor: 'older' } })
    expect(task.record.fact).toMatchObject({ kind: 'progress', progress: { history: [{ progress: { attempt: 4 } }, { progress: { attempt: 3 } }], nextCursor: null } })
    expect(task.record.readError).toBeNull()
  })

  it('reads all submission history pages using the submission endpoint', async () => {
    vi.mocked(getOwnProjectEvaluationTaskForSubmission).mockResolvedValueOnce(page('older', 4)).mockResolvedValueOnce(page(null, 3))
    const tasks = scope.run(() => useSubmissionTasks(ref('project')))!
    await tasks.refresh(['submission'])
    expect(getOwnProjectEvaluationTaskForSubmission).toHaveBeenNthCalledWith(2, { path: { projectId: 'project', submissionId: 'submission' }, query: { limit: 100, cursor: 'older' } })
    expect(tasks.records.submission?.fact).toMatchObject({ kind: 'progress', progress: { history: [{ progress: { attempt: 4 } }, { progress: { attempt: 3 } }] } })
  })

  it.each([false, 0, undefined, ''])('does not publish a partial progress reading after bad cursor %j', async (cursor) => {
    vi.mocked(getOwnProjectEvaluationTaskProgress).mockResolvedValueOnce(page('older', 4)).mockResolvedValueOnce(page(cursor, 3))
    const task = scope.run(() => useRunTask(ref('project'), ref('run')))!
    await task.refresh()
    expect(task.record.fact).toEqual({ kind: 'unread' })
    expect(task.record.readError).not.toBeNull()
  })
})
