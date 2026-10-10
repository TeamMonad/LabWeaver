import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import PlatformImageView from '@/views/admin/PlatformImageView.vue'
import {
  cancelPlatformImageUpload,
  completePlatformImageUpload,
  createPlatformImageUpload,
  disablePlatformImage,
  getPlatformImageUpload,
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
    getPlatformImageUpload: vi.fn(),
    cancelPlatformImageUpload: vi.fn(),
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

const vmEntry = {
  ...entry,
  catalogId: '0197f0e0-0000-7000-8000-000000000004',
  kind: 'virtual_machine' as const,
  binding: 'ubuntu-24.04-vm-v1',
  sourceReference: 'harbor.lab.lan/labweaver-system/ubuntu-vm:24.04',
  sizeBytes: 6442450944,
  capacityBytes: 10737418240,
  diskSha256: 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
  format: 'qcow2' as const,
  releaseReferenceCount: 1,
}

const uploadSession = {
  uploadId: '0197f0e0-0000-7000-8000-000000000002',
  kind: 'container' as const,
  binding: 'ubuntu-24.04-v1',
  targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
  archiveBytes: 7,
  archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
  uploadTarget: { partSizeBytes: 64 * 1024 * 1024, parts: [{ partNumber: 1, uploadUrl: 'https://objects.example.test/staged-archive', requiredHeaders: { 'x-amz-server-side-encryption': 'AES256' }, expiresAt: '2026-07-16T09:00:00.000Z' }], expiresAt: '2026-07-16T09:00:00.000Z' }, uploadedParts: [],
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
  Object.defineProperty(fileInput.element, 'files', { value: [file], configurable: true })
  await fileInput.trigger('change')
}

/** Selects the virtual-machine kind and fills the base-disk descriptor fields. */
async function fillVmUploadForm(
  wrapper: VueWrapper,
  file: File,
  descriptor: { capacity?: string; diskPath?: string; diskFormat?: string } = {},
) {
  const form = wrapper.get('.upload-card .admin-form')
  await form.get('select').setValue('virtual_machine')
  if (descriptor.diskFormat) await form.findAll('select')[1].setValue(descriptor.diskFormat)
  const inputs = form.findAll('input:not([type="file"])')
  await inputs[0].setValue('ubuntu-24.04-vm-v1')
  await inputs[1].setValue('harbor.lab.lan/labweaver-system/ubuntu-vm:24.04')
  await inputs[2].setValue('2')
  if (descriptor.capacity !== undefined) await inputs[3].setValue(descriptor.capacity)
  if (descriptor.diskPath !== undefined) await inputs[4].setValue(descriptor.diskPath)
  await form.get('textarea').setValue('导入已评审虚拟机模板')
  const fileInput = form.get('input[type="file"]')
  Object.defineProperty(fileInput.element, 'files', { value: [file], configurable: true })
  await fileInput.trigger('change')
}

describe('PlatformImageView', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    window.sessionStorage.clear()
    vi.mocked(listPlatformImages).mockResolvedValue({ data: { entries: [entry] }, error: undefined as never })
    vi.mocked(putFileWithProgress).mockResolvedValue({ etag: '"etag-default"' })
    vi.mocked(createPlatformImageUpload).mockResolvedValue({ data: uploadSession, error: undefined as never })
    vi.mocked(completePlatformImageUpload).mockResolvedValue({ data: entry, error: undefined as never })
    vi.mocked(getPlatformImageUpload).mockResolvedValue({
      data: { uploadId: uploadSession.uploadId, revision: 2, state: 'imported', catalogId: entry.catalogId },
      error: undefined as never,
    })
  })

  afterEach(() => {
    for (const wrapper of mounted.splice(0)) wrapper.unmount()
  })

  it('renders the catalog rows with the Control release impact hint', async () => {
    const wrapper = await mountView()

    expect(wrapper.text()).toContain('维护沙箱可用的容器与虚拟机基础镜像。digest 是权威身份，也是固定且不可变的镜像版本；tag 仅作解析入口。')
    expect(columnText(wrapper, 0, '类型')).toBe('容器')
    expect(columnText(wrapper, 0, 'binding')).toBe('ubuntu-24.04-v1')
    expect(columnText(wrapper, 0, 'digest')).toContain(entry.resolvedDigest.slice(0, 8))
    expect(columnText(wrapper, 0, '引用 release 数')).toBe('2')
    expect(columnText(wrapper, 0, '状态')).toBe('可用')
  })

  it('keeps field hints out of accessible names and associates them with their controls', async () => {
    const wrapper = await mountView()

    const registerTrust = wrapper.get('#register-trust-revision')
    expect(wrapper.get('label[for="register-trust-revision"]').text()).toBe('信任版本')
    expect(registerTrust.attributes('aria-describedby')).toBe('register-trust-revision-hint')
    expect(wrapper.find('label[for="register-trust-revision"] .field-hint').exists()).toBe(false)

    const uploadTrust = wrapper.get('#upload-trust-revision')
    expect(wrapper.get('label[for="upload-trust-revision"]').text()).toBe('信任版本')
    expect(uploadTrust.attributes('aria-describedby')).toBe('upload-trust-revision-hint')
    expect(wrapper.find('label[for="upload-trust-revision"] .field-hint').exists()).toBe(false)

    await wrapper.get('.upload-card select').setValue('virtual_machine')
    const diskFormat = wrapper.get('#upload-disk-format')
    expect(wrapper.get('label[for="upload-disk-format"]').text()).toBe('磁盘格式')
    expect(diskFormat.attributes('aria-describedby')).toBe('upload-disk-format-hint')
    expect(wrapper.find('label[for="upload-disk-format"] .field-hint').exists()).toBe(false)
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
    const containerBody = vi.mocked(createPlatformImageUpload).mock.calls[0][0].body
    expect(containerBody).not.toHaveProperty('diskFormat')
    expect(containerBody).not.toHaveProperty('diskPath')
    expect(containerBody).not.toHaveProperty('capacityBytes')
    expect(putFileWithProgress).toHaveBeenCalledWith(
      expect.any(Blob),
      uploadSession.uploadTarget.parts[0].uploadUrl,
      uploadSession.uploadTarget.parts[0].requiredHeaders,
      expect.any(Function),
      expect.any(AbortSignal),
    )
    const completion = vi.mocked(completePlatformImageUpload).mock.calls[0][0]
    expect(completion.path).toEqual({ uploadId: uploadSession.uploadId })
    expect(completion.headers).toEqual({ 'Idempotency-Key': expect.any(String), 'If-Match': '"rev-1"' })
    expect(listPlatformImages).toHaveBeenCalledTimes(2)
  })

  it('keeps the selected file and stages a new session when the object upload fails', async () => {
    vi.mocked(putFileWithProgress).mockRejectedValueOnce(new Error('对象存储上传失败：HTTP 503'))
    const wrapper = await mountView()
    const file = new File(['archive'], 'layout.tar', { type: 'application/vnd.oci.image.layout.v1+tar' })
    await fillUploadForm(wrapper, file)

    const form = wrapper.get('.upload-card .admin-form')
    await form.trigger('submit')
    await flushPromises()

    expect(wrapper.get('.diagnostic-banner').text()).toContain('对象存储上传失败：HTTP 503')
    expect(completePlatformImageUpload).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('已选择：layout.tar')

    vi.mocked(cancelPlatformImageUpload).mockResolvedValue({ data: {}, error: undefined as never })
    vi.mocked(getPlatformImageUpload).mockResolvedValueOnce({
      data: { uploadId: uploadSession.uploadId, revision: 2, state: 'cancelled' },
      error: undefined as never,
    })
    vi.mocked(getPlatformImageUpload).mockResolvedValue({
      data: { uploadId: uploadSession.uploadId, revision: 3, state: 'imported', catalogId: entry.catalogId },
      error: undefined as never,
    })
    await wrapper.get('.upload-status button').trigger('click')
    await flushPromises()

    await form.trigger('submit')
    await flushPromises()

    expect(createPlatformImageUpload).toHaveBeenCalledTimes(2)
    expect(putFileWithProgress).toHaveBeenCalledTimes(2)
    expect(listPlatformImages).toHaveBeenCalledTimes(2)
  })

  it('resumes an interrupted upload after refresh from a reselected file', async () => {
    const interruptedUploadId = '0197f0e0-0000-7000-8000-000000000011'
    window.sessionStorage.setItem('labweaver.platform-image-upload', JSON.stringify({
      uploadId: interruptedUploadId,
      revision: 2,
      completeIdempotencyKey: 'interrupted-complete-key',
      cancelIdempotencyKey: 'interrupted-cancel-key',
      phase: 'uploading',
      state: 'pending',
      archiveBytes: 7,
    }))
    vi.mocked(getPlatformImageUpload)
      .mockResolvedValueOnce({
        data: { uploadId: interruptedUploadId, revision: 2, state: 'pending', uploadTarget: uploadSession.uploadTarget, uploadedParts: [] },
        error: undefined as never,
      })
      .mockResolvedValueOnce({
        data: { uploadId: interruptedUploadId, revision: 2, state: 'pending', uploadTarget: uploadSession.uploadTarget, uploadedParts: [] },
        error: undefined as never,
      })
      .mockResolvedValueOnce({
        data: { uploadId: interruptedUploadId, revision: 3, state: 'imported', catalogId: entry.catalogId },
        error: undefined as never,
      })
    const wrapper = await mountView()

    expect(wrapper.get('.upload-status').text()).toContain('上传会话仍在等待归档。')
    expect(wrapper.find('.upload-file').exists()).toBe(false)

    const file = new File(['archive'], 'layout.tar', { type: 'application/vnd.oci.image.layout.v1+tar' })
    await fillUploadForm(wrapper, file)
    await wrapper.get('.upload-card .admin-form').trigger('submit')
    await flushPromises()

    expect(createPlatformImageUpload).not.toHaveBeenCalled()
    expect(putFileWithProgress).toHaveBeenCalledWith(
      expect.any(Blob),
      uploadSession.uploadTarget.parts[0].uploadUrl,
      uploadSession.uploadTarget.parts[0].requiredHeaders,
      expect.any(Function),
      expect.any(AbortSignal),
    )
    expect(vi.mocked(completePlatformImageUpload).mock.calls[0][0].path).toEqual({ uploadId: interruptedUploadId })
    expect(wrapper.get('.upload-status').text()).toContain('已导入')
  })

  it('shows an expired session and lets the administrator reselect without completing it again', async () => {
    const expiredUploadId = '0197f0e0-0000-7000-8000-000000000020'
    const replacementUploadId = '0197f0e0-0000-7000-8000-000000000021'
    window.sessionStorage.setItem('labweaver.platform-image-upload', JSON.stringify({
      uploadId: expiredUploadId,
      revision: 2,
      completeIdempotencyKey: 'expired-complete-key',
      cancelIdempotencyKey: 'expired-cancel-key',
      phase: 'uploading',
      state: 'pending',
      archiveBytes: 7,
    }))
    vi.mocked(getPlatformImageUpload)
      .mockResolvedValueOnce({
        data: {
          uploadId: expiredUploadId,
          revision: 3,
          state: 'failed',
          diagnostic: 'LW_PLATFORM_IMAGE_UPLOAD_EXPIRED',
        },
        error: undefined as never,
      })
      .mockResolvedValue({
        data: { uploadId: replacementUploadId, revision: 2, state: 'imported', catalogId: entry.catalogId },
        error: undefined as never,
      })
    vi.mocked(createPlatformImageUpload).mockResolvedValue({
      data: { ...uploadSession, uploadId: replacementUploadId, archiveBytes: 13 },
      error: undefined as never,
    })
    const wrapper = await mountView()

    expect(wrapper.get('.upload-status').text()).toContain('上传会话已过期，请重新选择归档文件上传。')
    expect(wrapper.get('.upload-diagnostic-code').text()).toBe('LW_PLATFORM_IMAGE_UPLOAD_EXPIRED')
    expect(wrapper.get('.upload-card .admin-form button[type="submit"]').element.hasAttribute('disabled')).toBe(true)
    expect(createPlatformImageUpload).not.toHaveBeenCalled()
    expect(completePlatformImageUpload).not.toHaveBeenCalled()
    expect(window.sessionStorage.getItem('labweaver.platform-image-upload')).toBeNull()

    await fillUploadForm(wrapper, new File(['fresh archive'], 'layout.tar'))
    expect(wrapper.get('.upload-card .admin-form button[type="submit"]').element.hasAttribute('disabled')).toBe(false)
    await wrapper.get('.upload-card .admin-form').trigger('submit')
    await flushPromises()

    expect(createPlatformImageUpload).toHaveBeenCalledOnce()
    expect(putFileWithProgress).toHaveBeenCalledOnce()
    expect(completePlatformImageUpload).toHaveBeenCalledOnce()
    expect(completePlatformImageUpload.mock.calls[0][0].path).toEqual({ uploadId: replacementUploadId })
    expect(wrapper.get('.upload-status').text()).toContain('已导入')
  })

  it('does not start a file PUT when an in-flight session creation resolves after the view unmounts', async () => {
    let resolveSession!: (value: unknown) => void
    const sessionResponse = new Promise((resolve) => { resolveSession = resolve })
    vi.mocked(createPlatformImageUpload).mockReturnValue(sessionResponse as never)
    const wrapper = await mountView()
    await fillUploadForm(wrapper, new File(['archive'], 'layout.tar'))
    await wrapper.get('.upload-card .admin-form').trigger('submit')
    await flushPromises()

    wrapper.unmount()
    resolveSession({ data: uploadSession, error: undefined as never })
    await flushPromises()

    expect(putFileWithProgress).not.toHaveBeenCalled()
    const saved = JSON.parse(window.sessionStorage.getItem('labweaver.platform-image-upload')!)
    expect(saved).toMatchObject({ uploadId: uploadSession.uploadId, phase: 'uploading', state: 'pending' })
    expect(saved).not.toHaveProperty('uploadTarget')
    expect(JSON.stringify(saved)).not.toContain(uploadSession.uploadTarget.parts[0].uploadUrl)
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

  it('renders the base-disk descriptor for virtual-machine rows and a dash for container rows', async () => {
    vi.mocked(listPlatformImages).mockResolvedValue({ data: { entries: [entry, vmEntry] }, error: undefined as never })
    const wrapper = await mountView()

    expect(columnText(wrapper, 0, '容量')).toBe('-')
    expect(columnText(wrapper, 0, 'disk_sha256')).toBe('-')
    expect(columnText(wrapper, 0, '格式')).toBe('-')
    expect(columnText(wrapper, 1, '容量')).toBe('10.00 GiB')
    expect(columnText(wrapper, 1, 'disk_sha256')).toBe(vmEntry.diskSha256.slice(0, 8) + '…' + vmEntry.diskSha256.slice(-8))
    expect(columnText(wrapper, 1, '格式')).toBe('qcow2')
  })

  it('accepts a virtual-machine archive and submits its disk descriptor', async () => {
    vi.mocked(createPlatformImageUpload).mockResolvedValue({ data: { ...uploadSession, archiveBytes: 8 }, error: undefined as never })
    const wrapper = await mountView()
    const file = new File(['template'], 'template.tar')

    expect(wrapper.get('.upload-card input[type="file"]').attributes('accept')).toBe('.tar')

    await fillVmUploadForm(wrapper, file, { capacity: '10737418240', diskFormat: 'raw' })

    expect(wrapper.get('.upload-card input[type="file"]').attributes('accept')).toBe('.tar,.tar.gz,.tgz')
    expect(wrapper.get('.upload-card').text()).toContain('包含单个 qcow2 或 raw 磁盘文件的归档')
    expect(wrapper.get('.upload-card').text()).toContain('不能直接上传裸磁盘文件或 OCI 布局')

    await wrapper.get('.upload-card .admin-form').trigger('submit')
    await flushPromises()

    expect(vi.mocked(createPlatformImageUpload).mock.calls[0][0].body).toEqual({
      kind: 'virtual_machine',
      binding: 'ubuntu-24.04-vm-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu-vm:24.04',
      trustRevision: 2,
      reason: '导入已评审虚拟机模板',
      archiveBytes: 8,
      archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
      diskFormat: 'raw',
      diskPath: 'disk/disk.img',
      capacityBytes: 10737418240,
    })
    expect(putFileWithProgress).toHaveBeenCalledOnce()
    expect(listPlatformImages).toHaveBeenCalledTimes(2)
  })

  it('blocks the virtual-machine upload before staging anything when the descriptor is missing or invalid', async () => {
    const wrapper = await mountView()
    const file = new File(['template'], 'template.tar')
    await fillVmUploadForm(wrapper, file)

    let form = wrapper.get('.upload-card .admin-form')
    await form.trigger('submit')
    await flushPromises()

    expect(createPlatformImageUpload).not.toHaveBeenCalled()
    expect(wrapper.get('.upload-card .diagnostic-banner').text()).toContain('PLATFORM_IMAGE_VM_DESCRIPTOR_INVALID')

    await fillVmUploadForm(wrapper, file, { capacity: '10737418240', diskPath: '../escape.img' })
    form = wrapper.get('.upload-card .admin-form')
    await form.trigger('submit')
    await flushPromises()

    expect(createPlatformImageUpload).not.toHaveBeenCalled()
    expect(wrapper.get('.upload-card .diagnostic-banner').text()).toContain('路径不得包含 `..`')
  })

  it('surfaces the Agent diagnostic when the virtual-machine import fails', async () => {
    vi.mocked(createPlatformImageUpload).mockResolvedValue({ data: { ...uploadSession, archiveBytes: 8 }, error: undefined as never })
    vi.mocked(completePlatformImageUpload).mockResolvedValue({
      error: { diagnosticCode: 'LW_PLATFORM_IMAGE_DISK_INVALID', detail: '归档中的磁盘与声明的路径不一致。' },
    } as never)
    vi.mocked(getPlatformImageUpload).mockResolvedValue({
      data: {
        uploadId: uploadSession.uploadId,
        revision: 2,
        state: 'failed',
        diagnostic: 'LW_PLATFORM_IMAGE_DISK_INVALID',
      },
      error: undefined as never,
    })
    const wrapper = await mountView()
    await fillVmUploadForm(wrapper, new File(['template'], 'template.tar'), { capacity: '4096' })

    await wrapper.get('.upload-card .admin-form').trigger('submit')
    await flushPromises()

    expect(completePlatformImageUpload).toHaveBeenCalled()
    expect(getPlatformImageUpload).toHaveBeenCalled()
    await vi.waitFor(() => {
      const status = wrapper.get('.upload-status').text()
      expect(status).toContain('镜像导入：导入失败')
      expect(status).toContain('镜像导入失败，请检查归档后重新上传。')
      expect(wrapper.get('.upload-diagnostic-code').text()).toContain('LW_PLATFORM_IMAGE_DISK_INVALID')
    })
    expect(columnText(wrapper, 0, 'binding')).toBe('ubuntu-24.04-v1')
  })
})
