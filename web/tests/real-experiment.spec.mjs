import { afterEach, describe, expect, it, vi } from 'vitest'

const approvalUi = vi.hoisted(() => ({ approve: vi.fn() }))

vi.mock('../e2e/support/real-resource.mjs', () => ({
  approveResourceRequestByUi: approvalUi.approve,
}))

import {
  approveAuthoringResourceRequestsByUi,
  waitForAuthoringRunWithResourceApproval,
} from '../e2e/support/real-experiment.mjs'

const RUN_ID = '00000000-0000-7000-8000-000000000001'
const OTHER_RUN_ID = '00000000-0000-7000-8000-000000000002'
const TASK_RUN_ID = '00000000-0000-7000-8000-000000000003'
const OTHER_TASK_RUN_ID = '00000000-0000-7000-8000-000000000004'
const PROJECT_ID = 'project'
const REQUESTER_ID = 'student'
const GIB = 1024 ** 3

function response(body, status = 200) {
  return {
    ok: () => status >= 200 && status < 300,
    status: () => status,
    text: async () => JSON.stringify(body),
  }
}

function compact(value) {
  return value.replaceAll('-', '')
}

function requestKey(runId = RUN_ID, track = 'environment', attempt = 1, taskRunId = TASK_RUN_ID) {
  return `authoring-${compact(runId)}-${track}-${attempt}-${compact(taskRunId)}`
}

function activeRun({ runId = RUN_ID, projectId = PROJECT_ID, attemptNumber = 1, attemptState = 'awaiting_approval', state = 'running' } = {}) {
  return {
    id: runId,
    projectId,
    state,
    tracks: [{ kind: 'environment', attempts: [{ number: attemptNumber, state: attemptState }] }],
  }
}

function taskRequest(overrides = {}) {
  return {
    id: 'resource-request',
    projectId: PROJECT_ID,
    requesterId: REQUESTER_ID,
    requestKey: requestKey(),
    target: { kind: 'task', taskRunId: TASK_RUN_ID },
    state: 'reviewing',
    requestedResources: {
      cpuMillicores: 1000,
      memoryBytes: GIB,
      storageBytes: GIB,
    },
    requestedDurationSeconds: 600,
    ...overrides,
  }
}

function adminPageFor(run, requests) {
  return {
    request: {
      get: vi.fn(async (path) => {
        if (path.includes('/agent-runs/')) return response(run)
        if (path === '/api/v1/resource-requests') return response(requests)
        throw new Error(`UNEXPECTED_READ:${path}`)
      }),
    },
  }
}

afterEach(() => {
  vi.clearAllMocks()
})

describe('real authoring resource approval recovery', () => {
  it('does not reject or re-approve an already approved first attempt', async () => {
    const run = activeRun({ attemptState: 'succeeded', state: 'succeeded' })
    const approved = taskRequest({ state: 'active' })
    const adminPage = adminPageFor(run, [approved])

    await expect(approveAuthoringResourceRequestsByUi(adminPage, run, REQUESTER_ID))
      .resolves.toBeUndefined()
    await expect(approveAuthoringResourceRequestsByUi(adminPage, run, REQUESTER_ID))
      .resolves.toBeUndefined()

    expect(approvalUi.approve).not.toHaveBeenCalled()
  })

  it('approves a reviewing request for the authoritative repair attempt', async () => {
    const staleRun = activeRun({ attemptNumber: 1, attemptState: 'failed', state: 'failed' })
    const repairedRun = activeRun({ attemptNumber: 2, attemptState: 'awaiting_approval' })
    const repairedRequest = taskRequest({
      requestKey: requestKey(RUN_ID, 'environment', 2, TASK_RUN_ID),
    })
    const adminPage = adminPageFor(repairedRun, [repairedRequest])
    approvalUi.approve.mockResolvedValue({ requestId: repairedRequest.id, leaseId: 'lease' })

    await expect(approveAuthoringResourceRequestsByUi(adminPage, staleRun, REQUESTER_ID, 'container-primary-v1'))
      .resolves.toBeUndefined()

    expect(approvalUi.approve).toHaveBeenCalledTimes(1)
    expect(approvalUi.approve).toHaveBeenCalledWith(adminPage, expect.objectContaining({
      requestKey: repairedRequest.requestKey,
      projectId: PROJECT_ID,
      requestId: repairedRequest.id,
      requesterId: REQUESTER_ID,
      durationSeconds: repairedRequest.requestedDurationSeconds,
      providerBinding: 'container-primary-v1',
    }))
  })

  it.each([
    ['other project', { projectId: 'foreign-project' }, 'REAL_EXPERIMENT_AUTHORING_RESOURCE_REQUEST_SCOPE_INVALID'],
    ['other requester', { requesterId: 'foreign-user' }, 'REAL_EXPERIMENT_AUTHORING_RESOURCE_REQUEST_SCOPE_INVALID'],
    ['other task run', { target: { kind: 'task', taskRunId: OTHER_TASK_RUN_ID } }, 'REAL_EXPERIMENT_AUTHORING_RESOURCE_REQUEST_SCOPE_INVALID'],
    ['other track', { requestKey: requestKey(RUN_ID, 'evaluation', 1, TASK_RUN_ID) }, 'REAL_EXPERIMENT_AUTHORING_RESOURCE_REQUEST_SCOPE_INVALID'],
    ['other attempt', { requestKey: requestKey(RUN_ID, 'environment', 2, TASK_RUN_ID) }, 'REAL_EXPERIMENT_AUTHORING_RESOURCE_REQUEST_SCOPE_INVALID'],
  ])('rejects a reviewing request with %s scope', async (_label, overrides, diagnostic) => {
    const run = activeRun()
    const adminPage = adminPageFor(run, [taskRequest(overrides)])

    await expect(approveAuthoringResourceRequestsByUi(adminPage, run, REQUESTER_ID))
      .rejects.toThrow(diagnostic)
    expect(approvalUi.approve).not.toHaveBeenCalled()
  })

  it('does not select a reviewing request belonging to another run', async () => {
    const run = activeRun()
    const foreignRequest = taskRequest({ requestKey: requestKey(OTHER_RUN_ID, 'environment', 1, TASK_RUN_ID) })
    const adminPage = adminPageFor(run, [foreignRequest])

    await expect(approveAuthoringResourceRequestsByUi(adminPage, run, REQUESTER_ID))
      .resolves.toBeUndefined()
    expect(approvalUi.approve).not.toHaveBeenCalled()
  })

  it('waits for the active attempt to finish after task-owner lease release', async () => {
    const active = activeRun({ attemptState: 'running', state: 'succeeded' })
    const finished = activeRun({ attemptState: 'succeeded', state: 'succeeded' })
    const studentRequest = {
      get: vi.fn()
        .mockResolvedValueOnce(response(active))
        .mockResolvedValueOnce(response(finished)),
    }
    const adminPage = adminPageFor(active, [taskRequest({ state: 'expired' })])

    const result = await waitForAuthoringRunWithResourceApproval({
      request: studentRequest,
      adminPage,
      projectId: PROJECT_ID,
      runId: RUN_ID,
      requesterId: REQUESTER_ID,
      timeout: 5_000,
    })

    expect(result).toEqual(finished)
    expect(studentRequest.get).toHaveBeenCalledTimes(2)
    expect(approvalUi.approve).not.toHaveBeenCalled()
  })
})
