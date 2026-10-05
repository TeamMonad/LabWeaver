import { nextTick, reactive } from 'vue'
import { mount, type VueWrapper } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import ResourceApprovalView from '@/views/admin/ResourceApprovalView.vue'
import type { ResourceRequestSchema } from '@/generated/contracts'

const mocks = vi.hoisted(() => ({
  useResourceApproval: vi.fn(),
}))

const projectMocks = vi.hoisted(() => ({
  projects: {
    kind: 'success' as const,
    data: [{ id: 'project-1', name: 'CUDA 实验项目' }],
  },
  selectedProjectId: null as string | null,
}))

vi.mock('@/composables/useResourceApproval', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/composables/useResourceApproval')>()
  return { ...actual, useResourceApproval: mocks.useResourceApproval }
})

vi.mock('@/composables/useProjects', () => ({
  useProjects: () => projectMocks,
}))

const cpuRequest = {
  id: 'request-cpu',
  requestKey: 'work-cpu',
  requesterId: 'teacher-1',
  courseId: 'course-1',
  projectId: 'project-1',
  generation: 1,
  revision: 3,
  requestedDurationSeconds: 3600,
  requestedResources: {
    cpuMillicores: 1000,
    memoryBytes: 1024 ** 3,
    storageBytes: 1024 ** 3,
  },
  target: {
    kind: 'environment',
    environmentId: 'environment-cpu',
    releaseId: 'release-1',
    releaseVersion: 1,
  },
  state: 'reviewing',
  diagnosticCode: null,
  createdAt: '2026-09-09T00:00:00.000Z',
  updatedAt: '2026-09-09T00:00:00.000Z',
} as ResourceRequestSchema

const gpuRequest = {
  ...cpuRequest,
  id: 'request-gpu',
  requestKey: 'work-gpu',
  requestedResources: {
    ...cpuRequest.requestedResources,
    gpu: { class: 'nvidia-t4', count: 1 },
  },
} as ResourceRequestSchema

const taskRequestOne = {
  ...cpuRequest,
  id: 'request-task-1',
  requestKey: 'evaluation-task-1',
  target: { kind: 'task', taskRunId: 'task-run-1' },
} as ResourceRequestSchema

const taskRequestTwo = {
  ...cpuRequest,
  id: 'request-task-2',
  requestKey: 'evaluation-task-2',
  target: { kind: 'task', taskRunId: 'task-run-2' },
} as ResourceRequestSchema

function approvalState(
  request: ResourceRequestSchema,
  providerOptions: Record<string, unknown>,
  requestList: ResourceRequestSchema[] = [request],
) {
  return reactive({
    requests: { kind: 'success', data: requestList },
    leases: { kind: 'empty' },
    providerOptions,
    selectedRequestId: request.id,
    selectedLeaseId: null,
    selectedRequest: request,
    selectedLease: null,
    requestResources: new Map([[request.id, request.requestedResources]]),
    acting: null,
    outcome: null,
    batchOutcome: null,
    refreshDiagnostic: null,
    load: vi.fn(),
    selectRequest: vi.fn(),
    selectLease: vi.fn(),
    runRequestAction: vi.fn().mockResolvedValue(true),
    runRequestActions: vi.fn().mockResolvedValue({ kind: 'success', items: [] }),
    renewLease: vi.fn().mockResolvedValue(true),
    revokeLease: vi.fn().mockResolvedValue(true),
  })
}

function mountView(
  request: ResourceRequestSchema,
  providerOptions: Record<string, unknown>,
  requestList: ResourceRequestSchema[] = [request],
) {
  const approval = approvalState(request, providerOptions, requestList)
  mocks.useResourceApproval.mockReturnValue(approval)
  const wrapper = mount(ResourceApprovalView, {
    global: {
      stubs: {
        DataTable: {
          props: ['rows'],
          template: '<div class="data-table-stub"><div v-for="row in rows" :key="row.id" class="request-row" @click="$emit(\'row-click\', row)"><slot name="selection" :row="row" /><span>{{ row.requestKey }}</span></div></div>',
        },
        DiagnosticBanner: { template: '<div />' },
        GcpStatusPill: { props: ['state'], template: '<span>{{ state }}</span>' },
        SvgIcon: { template: '<span />' },
        RouterLink: { props: ['to'], template: '<a class="router-link-stub" :data-path="to.path"><slot /></a>' },
        ConfirmDialog: {
          props: ['open', 'confirmText', 'description'],
          template: '<div v-if="open" role="alertdialog"><p>{{ description }}</p><button type="button" @click="$emit(\'confirm\')">{{ confirmText }}</button></div>',
        },
      },
    },
  })
  return { approval, wrapper }
}

async function openApproval(wrapper: VueWrapper, binding: string) {
  await wrapper.get('textarea[aria-label="资源申请操作理由"]').setValue('已核对资源与执行后端配置。')
  await wrapper.get('input[aria-label="执行后端绑定"]').setValue(binding)
  await nextTick()
}

describe('ResourceApprovalView provider binding rules', () => {
  beforeEach(() => {
    mocks.useResourceApproval.mockReset()
  })

  it('points administrators to the project resource request flow when none exist', () => {
    const { wrapper } = mountView(cpuRequest, { kind: 'empty' }, [])

    expect(wrapper.text()).toContain('暂无资源申请')
    expect(wrapper.text()).toContain('用户需要先在资源申请页选择项目并提交请求')
    expect(wrapper.get('.router-link-stub').attributes('data-path')).toBe('/researcher/resources')
    wrapper.unmount()
  })

  it('shows readable project and target context while keeping internal IDs advanced', () => {
    const { wrapper } = mountView(taskRequestOne, { kind: 'empty' })

    expect(wrapper.findAll('.request-detail .meta-row').some((row) => row.text().includes('评测任务资源'))).toBe(true)
    expect(wrapper.text()).toContain('CUDA 实验项目')
    expect(wrapper.text()).toContain('账号资料待同步')
    expect(wrapper.text()).toContain('查看内部标识与诊断')
    expect(wrapper.get('details.advanced-details').element.open).toBe(false)
    expect(wrapper.get('details.advanced-details').text()).toContain('task-run-1')
    wrapper.unmount()
  })

  it('explains task-owner cleanup without treating lease release as a successful task result', async () => {
    const { approval, wrapper } = mountView(cpuRequest, { kind: 'empty' })
    approval.selectedLease = {
      id: 'lease-task-owner-release',
      requestId: cpuRequest.id,
      claimId: 'claim-1',
      revision: 2,
      state: 'revoked',
      revokeReasonCode: 'task_owner_release',
      createdAt: '2026-09-09T00:00:00.000Z',
      updatedAt: '2026-09-09T00:00:00.000Z',
    } as never
    await nextTick()

    expect(wrapper.text()).toContain('临时任务资源已回收，请查看任务结果')
    expect(wrapper.text()).toContain('这不代表用户 Work 环境已释放')
    expect(wrapper.text()).not.toContain('任务已完成，临时资源已回收')
    wrapper.unmount()
  })

  it('explains a blocked active allocation without offering a misleading retry', () => {
    const { wrapper } = mountView({ ...cpuRequest, state: 'active', diagnosticCode: 'LW_RESOURCE_WORK_ALLOCATION_BLOCKED' }, { kind: 'empty' })
    expect(wrapper.get('[role="alert"]').text()).toContain('分配失败，请回收后重新申请。')
    expect(wrapper.text()).toContain('撤销这次授权')
    expect(wrapper.findAll('button').some((button) => button.text() === '重新送审')).toBe(false)
    wrapper.unmount()
  })

  it('offers rejected requests a confirmed resubmission instead of allocation retry', async () => {
    const rejected = { ...cpuRequest, state: 'rejected' as const }
    const { approval, wrapper } = mountView(rejected, { kind: 'empty' })
    await wrapper.get('textarea[aria-label="资源申请操作理由"]').setValue('已核对资源与执行后端配置。')
    await wrapper.findAll('button').find((button) => button.text() === '重新送审')!.trigger('click')
    expect(approval.runRequestAction).not.toHaveBeenCalled()
    await wrapper.get('[role="alertdialog"] button').trigger('click')
    expect(approval.runRequestAction).toHaveBeenCalledWith('retry', rejected.id, expect.objectContaining({ reason: '已核对资源与执行后端配置。' }), expect.objectContaining({ expectedRevision: rejected.revision }))
    wrapper.unmount()
  })

  it('allows a CPU-only approval with an empty GPU catalog after entering a binding', async () => {
    const { approval, wrapper } = mountView(cpuRequest, { kind: 'empty' })

    expect(wrapper.find('input[aria-label="执行后端绑定"]').exists()).toBe(true)
    expect(wrapper.find('select[aria-label="GPU Provider Binding"]').exists()).toBe(false)
    expect(wrapper.get('button.filled-button').element.disabled).toBe(true)

    await openApproval(wrapper, 'kubernetes-standard')
    expect(wrapper.get('button.filled-button').element.disabled).toBe(false)
    await wrapper.get('button.filled-button').trigger('click')
    await wrapper.get('[role="alertdialog"] button').trigger('click')

    expect(approval.runRequestAction).toHaveBeenCalledWith('approve', cpuRequest.id, expect.objectContaining({
      providerBinding: 'kubernetes-standard',
      resources: cpuRequest.requestedResources,
    }), expect.objectContaining({
      expectedRevision: cpuRequest.revision,
      expectedFingerprint: expect.any(String),
    }))
  })

  it('limits GPU approval to the matching active catalog provider', async () => {
    const { wrapper } = mountView(gpuRequest, {
      kind: 'success',
      data: [
        { providerBinding: 'gpu-t4', catalogEntryCount: 1, gpuClasses: ['nvidia-t4'] },
        { providerBinding: 'gpu-a10', catalogEntryCount: 1, gpuClasses: ['nvidia-a10'] },
      ],
    })

    const providerSelect = wrapper.get('select[aria-label="GPU Provider Binding"]')
    expect(wrapper.find('input[aria-label="执行后端绑定"]').exists()).toBe(false)
    expect(providerSelect.findAll('option').map((option) => option.element.value)).toEqual(['', 'gpu-t4'])
    await wrapper.get('textarea[aria-label="资源申请操作理由"]').setValue('已核对 GPU 容量目录。')
    expect(wrapper.get('button.filled-button').element.disabled).toBe(false)

    await providerSelect.setValue('')
    expect(wrapper.get('button.filled-button').element.disabled).toBe(true)
    await providerSelect.setValue('gpu-t4')
    expect(wrapper.get('button.filled-button').element.disabled).toBe(false)
  })

  it('requires explicit task selection and reports each batch approval outcome', async () => {
    const { approval, wrapper } = mountView(taskRequestOne, { kind: 'empty' }, [taskRequestOne, taskRequestTwo, cpuRequest])
    const batchCheckboxes = wrapper.findAll('input[aria-label^="选择任务请求"]')
    expect(batchCheckboxes).toHaveLength(2)
    expect(wrapper.text()).toContain('仅处理你勾选的待审批评测任务；每项操作前都会重新校验当前状态，单项失败会保留并单独显示。')

    await batchCheckboxes[0].setValue(true)
    await batchCheckboxes[1].setValue(true)
    await wrapper.get('input[aria-label="批量审批执行后端绑定"]').setValue('kubernetes-standard')
    await wrapper.get('textarea[aria-label="批量资源申请操作理由"]').setValue('逐项核对评测任务请求。')

    const result = {
      kind: 'partial' as const,
      items: [
        {
          requestId: taskRequestOne.id,
          kind: 'success' as const,
          diagnostic: { code: 'RESOURCE_REQUEST_APPROVED', message: '操作已接受。', retryable: false },
        },
        {
          requestId: taskRequestTwo.id,
          kind: 'error' as const,
          diagnostic: { code: 'RESOURCE_REQUEST_CHANGED_RESELECT', message: '请求已变化。', retryable: false },
        },
      ],
    }
    approval.runRequestActions.mockImplementation(async (_kind, _items) => {
      approval.batchOutcome = result
      return result
    })

    const batchButton = wrapper.findAll('button').find((button) => button.text().includes('批准已选择的 2 项'))!
    expect(batchButton.element.disabled).toBe(false)
    await batchButton.trigger('click')
    const dialog = wrapper.findAll('[role="alertdialog"]').find((candidate) => candidate.text().includes('确认批量批准'))!
    expect(dialog.text()).toContain('evaluation-task-1')
    expect(dialog.text()).toContain('evaluation-task-2')
    expect(dialog.text()).toContain('rev-3')
    await dialog.find('button').trigger('click')

    await nextTick()
    expect(approval.runRequestActions).toHaveBeenCalledWith('approve', [
      expect.objectContaining({
        requestId: taskRequestOne.id,
        expectedRevision: taskRequestOne.revision,
        expectedFingerprint: expect.any(String),
      }),
      expect.objectContaining({
        requestId: taskRequestTwo.id,
        expectedRevision: taskRequestTwo.revision,
        expectedFingerprint: expect.any(String),
      }),
    ])
    expect(wrapper.text()).toContain('批量审批结果：部分已受理')
    expect(wrapper.text()).toContain('RESOURCE_REQUEST_CHANGED_RESELECT')
  })

  it('filters by server returned task identifiers without inferring submission relations', async () => {
    const { wrapper } = mountView(taskRequestOne, { kind: 'empty' }, [taskRequestOne, taskRequestTwo, cpuRequest])

    expect(wrapper.findAll('.request-row')).toHaveLength(3)
    await wrapper.get('input[aria-label="按真实请求字段搜索"]').setValue('task-run-2')
    await nextTick()

    expect(wrapper.findAll('.request-row')).toHaveLength(1)
    expect(wrapper.find('.request-row').text()).toContain('evaluation-task-2')
    expect(wrapper.text()).toContain('结果只来自服务端返回的真实字段')
  })

  it('distinguishes a filtered no-match from an empty backend response and clears filters', async () => {
    const { wrapper } = mountView(taskRequestOne, { kind: 'empty' }, [taskRequestOne, taskRequestTwo, cpuRequest])

    await wrapper.get('input[aria-label="按真实请求字段搜索"]').setValue('missing-request')
    await nextTick()

    expect(wrapper.findAll('.request-row')).toHaveLength(0)
    expect(wrapper.text()).toContain('已加载资源申请，但当前筛选条件没有匹配项')
    await wrapper.get('.filter-empty button').trigger('click')
    await nextTick()

    expect(wrapper.findAll('.request-row')).toHaveLength(3)
    expect(wrapper.find('.filter-empty').exists()).toBe(false)
  })
})
