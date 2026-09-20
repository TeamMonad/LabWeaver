import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import PlatformImageView from '@/views/admin/PlatformImageView.vue'
import {
  completePlatformImageUpload,
  createPlatformImageUpload,
  disablePlatformImage,
  listPlatformImages,
  registerPlatformImage,
  repinPlatformImage,
} from '@/generated/contracts'
import { putFileWithProgress } from '@/utils/upload'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    listPlatformImages: vi.fn(),
    registerPlatformImage: vi.fn(),
    repinPlatformImage: vi.fn(),
    disablePlatformImage: vi.fn(),
    createPlatformImageUpload: vi.fn(),
    completePlatformImageUpload: vi.fn(),
  }
})

vi.mock('@/utils/upload', () => ({
  putFileWithProgress: vi.fn(),
}))

const entry = {
  catalogId: '0197f0e0-0000-7000-8000-000000000001',
  kind: 'container' as const,
  binding: 'ubuntu-24.04-v1',
  sourceReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
  resolvedDigest: `sha256:${'a'.repeat(64)}`,
  mediaType: 'application/vnd.oci.image.manifest.v1+json',
  sizeBytes: 4096,
  status: 'active' as const,
  trustRevision: 3,
  repinGeneration: 1,
  pinnedAt: '2026-07-16T08:00:00.000Z',
  updatedAt: '2026-07-16T08:00:00.000Z',
  releaseReferenceCount: 2,
}

const uploadSession = {
  uploadId: '0197f0e0-0000-7000-8000-000000000002',
  kind: 'container' as const,
  binding: 'ubuntu-24.04-v1',
  targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
  archiveBytes: 7,
  archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
  uploadTarget: {
    uploadUrl: 'https://objects.example.test/staged-archive',
    requiredHeaders: { 'x-amz-server-side-encryption': 'AES256' },
    expiresAt: '2026-07-16T09:00:00.000Z',
  },
  expiresAt: '2026-07-16T09:00:00.000Z',
  revision: 1,
}

const mounted: VueWrapper[] = []

async function mountView() {
  const wrapper = mount(PlatformImageView)
  mounted.push(wrapper)
  await flushPromises()
  return wrapper
}

function columnText(wrapper: VueWrapper, rowIndex: number, title: string): string {
  const column = wrapper.findAll('.catalog-table thead th').map((header) => header.text()).indexOf(title)
  if (column < 0) throw new Error(`missing column ${title}`)
  return wrapper.findAll('.catalog-table tbody tr')[rowIndex].findAll('td')[column].text()
}

function operationReasonInput(wrapper: VueWrapper) {
  return wrapper.findAll('.operation-credentials input')[0]
}

async function fillUploadForm(wrapper: VueWrapper, file: File) {
  const form = wrapper.get('.upload-card .admin-form')
  const inputs = form.findAll('input:not([type="file"])')
  await inputs[0].setValue('ubuntu-24.04-v1')
  await inputs[1].setValue('harbor.lab.lan/labweaver-system/ubuntu:24.04')
  await inputs[2].setValue('3')
  await form.get('textarea').setValue('导入已评审归档')
  const fileInput = form.get('input[type="file"]')
  Object.defineProperty(fileInput.element, 'files', { value: [file] })
  await fileInput.trigger('change')
}

describe('PlatformImageView', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    vi.mocked(listPlatformImages).mockResolvedValue({ data: { entries: [entry] }, error: undefined as never })
    vi.mocked(putFileWithProgress).mockResolvedValue(undefined)
    vi.mocked(createPlatformImageUpload).mockResolvedValue({ data: uploadSession, error: undefined as never })
    vi.mocked(completePlatformImageUpload).mockResolvedValue({ data: entry, error: undefined as never })
  })

  afterEach(() => {
    for (const wrapper of mounted.splice(0)) wrapper.unmount()
  })

  it('renders the catalog rows with the Control release impact hint', async () => {
    const wrapper = await mountView()

    expect(wrapper.text()).toContain('维护沙箱可用的容器与虚拟机基础镜像。digest 是权威身份，tag 仅作解析入口。')
    expect(columnText(wrapper, 0, '类型')).toBe('容器')
    expect(columnText(wrapper, 0, 'binding')).toBe('ubuntu-24.04-v1')
    expect(columnText(wrapper, 0, 'digest')).toContain(entry.resolvedDigest.slice(0, 8))
    expect(columnText(wrapper, 0, '引用 release 数')).toBe('2')
    expect(columnText(wrapper, 0, '状态')).toBe('可用')
  })

  it('disables the pinned digest only after the confirmation dialog is accepted', async () => {
    vi.mocked(disablePlatformImage).mockResolvedValue({ data: { ...entry, status: 'disabled' }, error: undefined as never })
    const wrapper = await mountView()
    await operationReasonInput(wrapper).setValue('该镜像存在未修复漏洞')

    const disableButton = wrapper.findAll('.row-actions button')[1]
    expect((disableButton.element as HTMLButtonElement).disabled).toBe(false)
    await disableButton.trigger('click')

    const dialog = document.body.querySelector('dialog')
    expect(dialog?.textContent).toContain('停用后新创作不再列出该镜像；已有 release 仍按其 digest 运行。当前有 2 个 release 引用该 digest。')
    expect(disablePlatformImage).not.toHaveBeenCalled()

    document.body.querySelector<HTMLButtonElement>('dialog .filled-button')?.click()
    await flushPromises()

    expect(vi.mocked(disablePlatformImage).mock.calls[0][0]).toEqual({
      path: { catalogId: entry.catalogId },
      headers: { 'Idempotency-Key': expect.any(String), 'If-Match': '*' },
      body: { expectedDigest: entry.resolvedDigest, reason: '该镜像存在未修复漏洞' },
    })
    expect(listPlatformImages).toHaveBeenCalledTimes(2)
  })

  it('keeps the row actions disabled until an operation reason is recorded', async () => {
    const wrapper = await mountView()

    expect(wrapper.findAll('.row-actions button').every((button) => (button.element as HTMLButtonElement).disabled)).toBe(true)
    expect(wrapper.text()).toContain('填写操作原因后可执行「重新固定」或「停用」。')

    await operationReasonInput(wrapper).setValue('  记录原因  ')

    expect(wrapper.findAll('.row-actions button').every((button) => (button.element as HTMLButtonElement).disabled)).toBe(false)
  })

  it('repins at the recorded trust revision after confirmation', async () => {
    vi.mocked(repinPlatformImage).mockResolvedValue({ data: entry, error: undefined as never })
    const wrapper = await mountView()
    await operationReasonInput(wrapper).setValue('跟随已评审的安全更新')
    const trustRevisionInput = wrapper.findAll('.operation-credentials input')[1]
    await trustRevisionInput.setValue('4')

    await wrapper.findAll('.row-actions button')[0].trigger('click')

    const dialog = document.body.querySelector('dialog')
    expect(dialog?.textContent).toContain('将按已存引用重新解析并替换固定 digest；下游只认 digest，不会自动跟随 tag。')

    document.body.querySelector<HTMLButtonElement>('dialog .filled-button')?.click()
    await flushPromises()

    expect(vi.mocked(repinPlatformImage).mock.calls[0][0]).toEqual({
      path: { catalogId: entry.catalogId },
      headers: { 'Idempotency-Key': expect.any(String), 'If-Match': '*' },
      body: { expectedDigest: entry.resolvedDigest, trustRevision: 4, reason: '跟随已评审的安全更新' },
    })
  })

  it('registers a registry reference with the reviewed request body', async () => {
    vi.mocked(registerPlatformImage).mockResolvedValue({ data: entry, error: undefined as never })
    const wrapper = await mountView()
    const form = wrapper.get('.register-card .admin-form')
    const inputs = form.findAll('input')
    await form.get('select').setValue('virtual_machine')
    await inputs[0].setValue('ubuntu-24.04-v1')
    await inputs[1].setValue('harbor.lab.lan/labweaver-system/ubuntu:24.04')
    await inputs[2].setValue('5')
    await form.get('textarea').setValue('登记虚拟机基础镜像')

    await form.trigger('submit')
    await flushPromises()

    expect(vi.mocked(registerPlatformImage).mock.calls[0][0].body).toEqual({
      kind: 'virtual_machine',
      binding: 'ubuntu-24.04-v1',
      sourceReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 5,
      reason: '登记虚拟机基础镜像',
    })
  })

  it('uploads the selected archive through a freshly staged session and completes the import', async () => {
    const wrapper = await mountView()
    const file = new File(['archive'], 'layout.tar', { type: 'application/vnd.oci.image.layout.v1+tar' })
    await fillUploadForm(wrapper, file)

    await wrapper.get('.upload-card .admin-form').trigger('submit')
    await flushPromises()

    expect(vi.mocked(createPlatformImageUpload).mock.calls[0][0].body).toEqual({
      kind: 'container',
      binding: 'ubuntu-24.04-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 3,
      reason: '导入已评审归档',
      archiveBytes: 7,
      archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
    })
    expect(putFileWithProgress).toHaveBeenCalledWith(
      file,
      uploadSession.uploadTarget.uploadUrl,
      uploadSession.uploadTarget.requiredHeaders,
      expect.any(Function),
    )
    const completion = vi.mocked(completePlatformImageUpload).mock.calls[0][0]
    expect(completion.path).toEqual({ uploadId: uploadSession.uploadId })
    expect(completion.headers).toEqual({ 'Idempotency-Key': expect.any(String), 'If-Match': '"rev-1"' })
    expect(listPlatformImages).toHaveBeenCalledTimes(2)
  })

  it('keeps the selected file and stages a new session when the object upload fails', async () => {
    vi.mocked(putFileWithProgress).mockRejectedValueOnce(new Error('上传失败：403 Forbidden'))
    const wrapper = await mountView()
    const file = new File(['archive'], 'layout.tar', { type: 'application/vnd.oci.image.layout.v1+tar' })
    await fillUploadForm(wrapper, file)

    const form = wrapper.get('.upload-card .admin-form')
    await form.trigger('submit')
    await flushPromises()

    expect(wrapper.get('.diagnostic-banner').text()).toContain('上传并导入 OCI 归档失败。')
    expect(completePlatformImageUpload).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('已选择：layout.tar')

    await form.trigger('submit')
    await flushPromises()

    expect(createPlatformImageUpload).toHaveBeenCalledTimes(2)
    expect(putFileWithProgress).toHaveBeenCalledTimes(2)
    expect(listPlatformImages).toHaveBeenCalledTimes(2)
  })

  it('shows the upstream diagnostic code and keeps the catalog when a mutation conflicts', async () => {
    vi.mocked(disablePlatformImage).mockResolvedValue({
      error: { diagnosticCode: 'LW_PLATFORM_IMAGE_STATE_CONFLICT', detail: '该镜像已被其他管理员修改。' },
    } as never)
    const wrapper = await mountView()
    await operationReasonInput(wrapper).setValue('停用过期镜像')

    await wrapper.findAll('.row-actions button')[1].trigger('click')
    document.body.querySelector<HTMLButtonElement>('dialog .filled-button')?.click()
    await flushPromises()

    expect(wrapper.get('.diagnostic-banner').text()).toContain('LW_PLATFORM_IMAGE_STATE_CONFLICT')
    expect(columnText(wrapper, 0, 'binding')).toBe('ubuntu-24.04-v1')
  })
})
