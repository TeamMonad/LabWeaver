import { defineComponent, h } from 'vue'
import { mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { requestFingerprint, useResourceApproval } from '@/composables/useResourceApproval'
import type { ResourceRequestSchema } from '@/generated/contracts'

const mocks = vi.hoisted(() => ({
  apiGet: vi.fn(),
  approveResourceRequest: vi.fn(),
  getResourceLease: vi.fn(),
  getResourceRequest: vi.fn(),
  listResourceLeases: vi.fn(),
  listResourceRequests: vi.fn(),
  rejectResourceRequest: vi.fn(),
  renewResourceLease: vi.fn(),
  resizeAndApproveResourceRequest: vi.fn(),
  retryResourceRequest: vi.fn(),
  revokeResourceLease: vi.fn(),
}))

vi.mock('@/api/client', () => ({ apiClient: { get: mocks.apiGet } }))

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, ...mocks }
})

function success<T>(data: T) {
  return { data, error: undefined as never }
}

function taskRequest(id: string, revision = 3): ResourceRequestSchema {
  return {
    id,
    generation: 1,
    requestKey: `task-${id}`,
    requesterId: 'student-1',
    courseId: 'course-1',
    projectId: 'project-1',
    target: { kind: 'task', taskRunId: `task-run-${id}` },
    requestedResources: {
      cpuMillicores: 1000,
      memoryBytes: 1024 ** 3,
      storageBytes: 1024 ** 3,
    },
    requestedDurationSeconds: 1800,
    state: 'reviewing',
    revision,
    diagnosticCode: null,
    createdAt: '2026-09-14T00:00:00.000Z',
    updatedAt: '2026-09-14T00:00:00.000Z',
  }
}

function mountApproval() {
  let state!: ReturnType<typeof useResourceApproval>
  const Harness = defineComponent({
    setup() {
      state = useResourceApproval()
      return () => h('div')
    },
  })
  const wrapper = mount(Harness)
  return { state, wrapper }
}

afterEach(() => {
  vi.useRealTimers()
  Object.values(mocks).forEach((mock) => mock.mockReset())
})

describe('useResourceApproval', () => {
  it('checks every selected task request against its latest content and revision before approval', async () => {
    const first = taskRequest('one')
    const second = taskRequest('two')
    mocks.listResourceRequests.mockResolvedValue(success([first, second]))
    mocks.listResourceLeases.mockResolvedValue(success([]))
    mocks.apiGet.mockResolvedValue(success([]))
    mocks.getResourceRequest.mockImplementation(({ path }: { path: { requestId: string } }) => (
      Promise.resolve(success(path.requestId === first.id ? first : { ...second, revision: 4 }))
    ))
    mocks.approveResourceRequest.mockResolvedValue(success({ requestId: first.id, leaseId: 'lease-one', revision: 4, statusUrl: '/resource/one' }))

    const { state, wrapper } = mountApproval()
    await vi.waitFor(() => expect(state.requests).toEqual({ kind: 'success', data: [first, second] }))

    const result = await state.runRequestActions('approve', [
      {
        requestId: first.id,
        expectedRevision: first.revision,
        expectedFingerprint: requestFingerprint(first),
        payload: {
          providerBinding: 'kubernetes-standard',
          resources: first.requestedResources,
          durationSeconds: first.requestedDurationSeconds,
          reason: '逐项核对 task 请求。',
        },
      },
      {
        requestId: second.id,
        expectedRevision: second.revision,
        expectedFingerprint: requestFingerprint(second),
        payload: {
          providerBinding: 'kubernetes-standard',
          resources: second.requestedResources,
          durationSeconds: second.requestedDurationSeconds,
          reason: '逐项核对 task 请求。',
        },
      },
    ])

    expect(result.kind).toBe('partial')
    expect(result.items.map((item) => [item.requestId, item.kind])).toEqual([
      [first.id, 'success'],
      [second.id, 'error'],
    ])
    expect(mocks.getResourceRequest).toHaveBeenCalledTimes(2)
    expect(mocks.approveResourceRequest).toHaveBeenCalledTimes(1)
    expect(mocks.approveResourceRequest).toHaveBeenCalledWith(expect.objectContaining({
      path: { requestId: first.id },
      headers: { 'If-Match': '"rev-3"', 'Idempotency-Key': expect.any(String) },
      body: expect.objectContaining({ expectedRevision: 3 }),
    }))
    expect(result.items[1].diagnostic.code).toBe('RESOURCE_REQUEST_CHANGED_RESELECT')
    wrapper.unmount()
  })

  it('does not approve a confirmed v1 snapshot after the request becomes v2', async () => {
    const versionOne = taskRequest('stale', 1)
    const versionTwo = {
      ...versionOne,
      revision: 2,
      requestedDurationSeconds: 3600,
      updatedAt: '2026-09-14T00:01:00.000Z',
    }
    mocks.listResourceRequests.mockResolvedValue(success([versionOne]))
    mocks.listResourceLeases.mockResolvedValue(success([]))
    mocks.apiGet.mockResolvedValue(success([]))
    mocks.getResourceRequest.mockResolvedValue(success(versionTwo))

    const { state, wrapper } = mountApproval()
    await vi.waitFor(() => expect(state.requests).toEqual({ kind: 'success', data: [versionOne] }))

    const result = await state.runRequestActions('approve', [{
      requestId: versionOne.id,
      expectedRevision: versionOne.revision,
      expectedFingerprint: requestFingerprint(versionOne),
      payload: {
        providerBinding: 'kubernetes-standard',
        resources: versionOne.requestedResources,
        durationSeconds: versionOne.requestedDurationSeconds,
        reason: '确认 v1 后提交。',
      },
    }])

    expect(result.kind).toBe('error')
    expect(result.items[0]?.diagnostic.code).toBe('RESOURCE_REQUEST_CHANGED_RESELECT')
    expect(mocks.approveResourceRequest).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('keeps previously displayed data and warns when a quiet refresh is partial', async () => {
    vi.useFakeTimers()
    const request = taskRequest('one')
    mocks.listResourceRequests
      .mockResolvedValueOnce(success([request]))
      .mockResolvedValueOnce(success([request]))
      .mockRejectedValue(new Error('request refresh offline'))
    mocks.listResourceLeases.mockResolvedValue(success([]))
    mocks.apiGet.mockResolvedValue(success([]))

    const { state, wrapper } = mountApproval()
    await vi.waitFor(() => expect(state.requests).toEqual({ kind: 'success', data: [request] }))

    await state.load()
    await vi.advanceTimersByTimeAsync(5000)
    await vi.waitFor(() => expect(state.refreshDiagnostic?.code).toBe('RESOURCE_REFRESH_PARTIAL'))

    expect(state.requests).toEqual({ kind: 'success', data: [request] })
    expect(state.refreshDiagnostic?.message).toContain('RESOURCE_REQUEST_REFRESH_FAILED')
    wrapper.unmount()
  })
})
