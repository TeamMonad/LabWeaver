import { nextTick, reactive } from 'vue'
import { mount, type VueWrapper } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import ResourceApprovalView from '@/views/admin/ResourceApprovalView.vue'
import type { ResourceRequestSchema } from '@/generated/contracts'

const mocks = vi.hoisted(() => ({
  useResourceApproval: vi.fn(),
}))

vi.mock('@/composables/useResourceApproval', () => ({
  useResourceApproval: mocks.useResourceApproval,
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

function approvalState(request: ResourceRequestSchema, providerOptions: Record<string, unknown>) {
  return reactive({
    requests: { kind: 'success', data: [request] },
    leases: { kind: 'empty' },
    providerOptions,
    selectedRequestId: request.id,
    selectedLeaseId: null,
    selectedRequest: request,
    selectedLease: null,
    requestResources: new Map([[request.id, request.requestedResources]]),
    acting: null,
    outcome: null,
    load: vi.fn(),
    selectRequest: vi.fn(),
    selectLease: vi.fn(),
    runRequestAction: vi.fn().mockResolvedValue(true),
    renewLease: vi.fn().mockResolvedValue(true),
    revokeLease: vi.fn().mockResolvedValue(true),
  })
}

function mountView(request: ResourceRequestSchema, providerOptions: Record<string, unknown>) {
  const approval = approvalState(request, providerOptions)
  mocks.useResourceApproval.mockReturnValue(approval)
  const wrapper = mount(ResourceApprovalView, {
    global: {
      stubs: {
        DataTable: {
          props: ['rows'],
          template: '<div class="data-table-stub"><span v-for="row in rows" :key="row.id">{{ row.requestKey }}</span></div>',
        },
        DiagnosticBanner: { template: '<div />' },
        GcpStatusPill: { props: ['state'], template: '<span>{{ state }}</span>' },
        SvgIcon: { template: '<span />' },
        ConfirmDialog: {
          props: ['open', 'confirmText'],
          template: '<div v-if="open" role="alertdialog"><button type="button" @click="$emit(\'confirm\')">{{ confirmText }}</button></div>',
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
})
