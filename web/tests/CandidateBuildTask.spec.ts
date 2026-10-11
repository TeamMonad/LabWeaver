import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { ref } from 'vue'
import CandidateBuildTask from '@/components/common/CandidateBuildTask.vue'
import { cancelProjectCandidateBuild, getProjectCandidateBuild } from '@/generated/contracts'

vi.mock('@/generated/contracts', () => ({ getProjectCandidateBuild: vi.fn(), cancelProjectCandidateBuild: vi.fn() }))
const actor = vi.hoisted(() => ({ authenticated: true, roles: ['teacher'] as string[] }))
vi.mock('@/composables/useAuth', () => ({ useAuth: () => ({ isAuthenticated: ref(actor.authenticated), user: ref({ profile: { roles: actor.roles } }) }) }))
const getBuild = vi.mocked(getProjectCandidateBuild)
const cancelBuild = vi.mocked(cancelProjectCandidateBuild)
function task(candidateId = 'candidate-1', state = 'running', revision = 2, cancellationRequested = false, cleanupVerified: boolean | null = null) {
  return { candidateId, target: 'environment', status: { projectId: 'project-1', courseId: null, buildRequestId: `build-${candidateId}`,
    state, revision, cancellationRequested, cleanupVerified, diagnosticCode: null } }
}
function result(data: ReturnType<typeof task>) { return { data, response: { status: 200 } } as never }
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((r) => { resolve = r }); return { promise, resolve } }
beforeEach(() => { vi.useFakeTimers(); vi.resetAllMocks(); actor.authenticated = true; actor.roles = ['teacher'] })
afterEach(() => { vi.useRealTimers() })

describe('candidate build authority', () => {
  it('requests exact task cancellation and keeps recycling until the authority confirms cleanup', async () => {
    getBuild.mockResolvedValueOnce(result(task())).mockResolvedValueOnce(result(task('candidate-1', 'cancelled', 4, true, false)))
      .mockResolvedValueOnce(result(task('candidate-1', 'cancelled', 5, true, true)))
    cancelBuild.mockResolvedValue(result(task('candidate-1', 'running', 3, true)))
    const wrapper = mount(CandidateBuildTask, { props: { projectId: 'project-1', candidateId: 'candidate-1', target: 'environment' } })
    await flushPromises()
    await wrapper.find('button').trigger('click'); await flushPromises()
    expect(cancelBuild).toHaveBeenCalledWith(expect.objectContaining({
      path: { projectId: 'project-1', candidateId: 'candidate-1', target: 'environment' },
      body: { buildRequestId: 'build-candidate-1', expectedState: 'running', expectedRevision: 2 },
      headers: { 'If-Match': '"rev-2"', 'Idempotency-Key': expect.any(String) },
    }))
    expect(wrapper.text()).toContain('仍在回收中'); expect(wrapper.text()).not.toContain('已回收')
    expect(wrapper.findAll('button')).toHaveLength(0)
    await vi.advanceTimersByTimeAsync(3000); await flushPromises()
    expect(wrapper.text()).toContain('已回收'); expect(wrapper.text()).not.toContain('仍在回收中')
    wrapper.unmount()
  })
  it('does not let an old candidate read or cancel overwrite a changed route', async () => {
    const oldRead = deferred<never>(); getBuild.mockReturnValueOnce(oldRead.promise).mockResolvedValueOnce(result(task('candidate-2')))
    const wrapper = mount(CandidateBuildTask, { props: { projectId: 'project-1', candidateId: 'candidate-1', target: 'environment' } })
    await wrapper.setProps({ candidateId: 'candidate-2' }); await flushPromises()
    oldRead.resolve(result(task('candidate-1', 'cancelled', 4, true, true))); await flushPromises()
    expect(wrapper.text()).toContain('构建中'); expect(wrapper.text()).not.toContain('已取消')
    const oldCancel = deferred<never>(); cancelBuild.mockReturnValueOnce(oldCancel.promise)
    await wrapper.find('button').trigger('click')
    getBuild.mockResolvedValueOnce(result(task('candidate-3')))
    await wrapper.setProps({ candidateId: 'candidate-3' }); await flushPromises()
    oldCancel.resolve(result(task('candidate-2', 'cancelled', 4, true, true))); await flushPromises()
    expect(wrapper.text()).toContain('构建中'); expect(wrapper.text()).not.toContain('已回收')
    wrapper.unmount()
  })
  it('reuses one exact command after a lost response and clears it when polling confirms cancellation', async () => {
    getBuild.mockResolvedValue(result(task()))
    cancelBuild.mockResolvedValueOnce({ error: new Error('connection lost') } as never)
      .mockResolvedValueOnce(result(task('candidate-1', 'running', 3, true)))
    const wrapper = mount(CandidateBuildTask, { props: { projectId: 'project-1', candidateId: 'candidate-1', target: 'environment' } })
    await flushPromises(); await wrapper.find('button').trigger('click'); await flushPromises()
    expect(wrapper.text()).toContain('重新提交同一次取消请求')
    getBuild.mockResolvedValue(result(task('candidate-1', 'running', 3, true)))
    await wrapper.findAll('button').find(b => b.text().includes('同一次'))!.trigger('click'); await flushPromises()
    expect(cancelBuild.mock.calls[0]?.[0]).toEqual(cancelBuild.mock.calls[1]?.[0])
    expect(wrapper.text()).toContain('取消中'); expect(wrapper.text()).not.toContain('同一次')
    wrapper.unmount()
  })
  it('shows no cancellable task before enqueue and refuses cross-project results', async () => {
    getBuild.mockResolvedValueOnce({ error: { diagnosticCode: 'NOT_FOUND' }, response: { status: 404 } } as never)
    const wrapper = mount(CandidateBuildTask, { props: { projectId: 'project-1', candidateId: 'candidate-1', target: 'environment' } })
    await flushPromises(); expect(wrapper.text()).toContain('尚未开始'); expect(wrapper.findAll('button')).toHaveLength(0)
    const wrong = task(); wrong.status.projectId = 'foreign'
    getBuild.mockResolvedValueOnce(result(wrong)); await vi.advanceTimersByTimeAsync(3000); await flushPromises()
    expect(wrapper.text()).toContain('构建任务引用已变化'); expect(wrapper.findAll('button')).toHaveLength(0)
    wrapper.unmount()
  })
  it.each([400, 403, 404, 409, 410, 422, 503])('does not retry an explicit rejected cancellation (%i)', async (status) => {
    getBuild.mockResolvedValue(result(task()))
    cancelBuild.mockResolvedValue({ error: { diagnosticCode: 'LW_AGENT_BUILD_STATE_CONFLICT', detail: '本次取消已拒绝', retryable: false }, response: { status } } as never)
    const wrapper = mount(CandidateBuildTask, { props: { projectId: 'project-1', candidateId: 'candidate-1', target: 'environment' } })
    await flushPromises(); await wrapper.find('button').trigger('click'); await flushPromises()
    expect(wrapper.text()).toContain('本次取消已拒绝'); expect(wrapper.text()).not.toContain('同一次取消请求')
    wrapper.unmount()
  })
  it('clears an old task when it is no longer found and uses existing authenticated roles', async () => {
    getBuild.mockResolvedValueOnce(result(task())).mockResolvedValueOnce({ error: {}, response: { status: 404 } } as never)
    const wrapper = mount(CandidateBuildTask, { props: { projectId: 'project-1', candidateId: 'candidate-1', target: 'environment' } })
    await flushPromises(); expect(wrapper.findAll('button')).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(3000); await flushPromises()
    expect(wrapper.text()).toContain('尚未开始'); expect(wrapper.findAll('button')).toHaveLength(0)
    wrapper.unmount()
    actor.roles = []; getBuild.mockResolvedValue(result(task()))
    const unauthorized = mount(CandidateBuildTask, { props: { projectId: 'project-1', candidateId: 'candidate-1', target: 'environment' } })
    await flushPromises(); expect(unauthorized.findAll('button')).toHaveLength(0)
    unauthorized.unmount()
  })

})
