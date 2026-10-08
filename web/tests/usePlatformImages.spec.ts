import { describe, it, expect, vi, beforeEach } from 'vitest'
import { defineComponent, h } from 'vue'
import { mount } from '@vue/test-utils'
import { usePlatformImages } from '@/composables/usePlatformImages'
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

function problem(diagnosticCode: string, detail: string) {
  return { diagnosticCode, detail, retryable: false }
}

describe('usePlatformImages', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    window.sessionStorage.clear()
    vi.mocked(listPlatformImages).mockResolvedValue({ data: { entries: [entry] }, error: undefined as never })
    vi.mocked(putFileWithProgress).mockResolvedValue({ etag: '"etag-default"' })
  })

  it('renders the server catalog projection after load', async () => {
    const images = usePlatformImages()

    await images.load()

    expect(listPlatformImages).toHaveBeenCalledWith()
    expect(images.state).toEqual({ kind: 'ready' })
    expect(images.entries).toEqual([entry])
  })

  it('registers with a fresh idempotency key and reloads the catalog', async () => {
    vi.mocked(registerPlatformImage).mockResolvedValue({ data: entry, error: undefined as never })
    const images = usePlatformImages()

    await expect(images.register({
      kind: 'container',
      binding: 'ubuntu-24.04-v1',
      sourceReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 3,
      reason: '首次登记基础镜像',
    })).resolves.toBe(true)

    const request = vi.mocked(registerPlatformImage).mock.calls[0][0]
    expect(request.body).toEqual({
      kind: 'container',
      binding: 'ubuntu-24.04-v1',
      sourceReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 3,
      reason: '首次登记基础镜像',
    })
    expect(request.headers['Idempotency-Key']).toBeTruthy()
    expect(listPlatformImages).toHaveBeenCalledTimes(1)
    expect(images.entries).toEqual([entry])
  })

  it('repins the digest the row currently pins and reloads the catalog', async () => {
    vi.mocked(repinPlatformImage).mockResolvedValue({ data: entry, error: undefined as never })
    const images = usePlatformImages()

    await expect(images.repin(entry, 4, '跟随已评审的安全更新')).resolves.toBe(true)

    const request = vi.mocked(repinPlatformImage).mock.calls[0][0]
    expect(request.path).toEqual({ catalogId: entry.catalogId })
    expect(request.body).toEqual({ expectedDigest: entry.resolvedDigest, trustRevision: 4, reason: '跟随已评审的安全更新' })
    expect(request.headers).toEqual({ 'Idempotency-Key': expect.any(String), 'If-Match': '*' })
    expect(listPlatformImages).toHaveBeenCalledTimes(1)
  })

  it('disables the digest the row currently pins and reloads the catalog', async () => {
    vi.mocked(disablePlatformImage).mockResolvedValue({ data: entry, error: undefined as never })
    const images = usePlatformImages()

    await expect(images.disable(entry, '该镜像存在未修复漏洞')).resolves.toBe(true)

    const request = vi.mocked(disablePlatformImage).mock.calls[0][0]
    expect(request.path).toEqual({ catalogId: entry.catalogId })
    expect(request.body).toEqual({ expectedDigest: entry.resolvedDigest, reason: '该镜像存在未修复漏洞' })
    expect(request.headers).toEqual({ 'Idempotency-Key': expect.any(String), 'If-Match': '*' })
    expect(listPlatformImages).toHaveBeenCalledTimes(1)
  })

  it('keeps the loaded catalog and surfaces the diagnostic when a mutation fails', async () => {
    vi.mocked(disablePlatformImage).mockResolvedValue({
      error: problem('LW_PLATFORM_IMAGE_STATE_CONFLICT', '该镜像已被其他管理员修改。'),
    } as never)
    const images = usePlatformImages()
    await images.load()

    await expect(images.disable(entry, '停用')).resolves.toBe(false)

    expect(images.entries).toEqual([entry])
    expect(images.state).toEqual({
      kind: 'error',
      diagnostic: { code: 'LW_PLATFORM_IMAGE_STATE_CONFLICT', message: '该镜像已被其他管理员修改。', retryable: false },
    })
    expect(listPlatformImages).toHaveBeenCalledTimes(1)

    images.clearDiagnostic()
    expect(images.state).toEqual({ kind: 'ready' })
  })

  it('uploads the archive to the staged target and completes the import', async () => {
    const session = {
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
    vi.mocked(createPlatformImageUpload).mockResolvedValue({ data: session, error: undefined as never })
    vi.mocked(completePlatformImageUpload).mockResolvedValue({ data: entry, error: undefined as never })
    vi.mocked(getPlatformImageUpload).mockResolvedValue({
      data: { uploadId: session.uploadId, revision: 2, state: 'imported', catalogId: entry.catalogId },
      error: undefined as never,
    })
    const images = usePlatformImages()
    let observedProgress: number | null = null
    vi.mocked(putFileWithProgress).mockImplementation(async (_file, _url, _headers, onProgress) => {
      onProgress(42)
      observedProgress = images.state.kind === 'uploading' ? images.state.progress : null
      return { etag: '"etag-1"' }
    })
    const file = new File(['archive'], 'layout.tar', { type: 'application/vnd.oci.image.layout.v1+tar' })

    await expect(images.upload(file, {
      kind: 'container',
      binding: 'ubuntu-24.04-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 3,
      reason: '导入已评审归档',
    })).resolves.toBe(true)

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
      session.uploadTarget.parts[0].uploadUrl,
      session.uploadTarget.parts[0].requiredHeaders,
      expect.any(Function),
      expect.any(AbortSignal),
    )
    expect(vi.mocked(putFileWithProgress).mock.calls[0][0]).toEqual(expect.objectContaining({ size: file.size }))
    expect(observedProgress).toBe(43)
    const completion = vi.mocked(completePlatformImageUpload).mock.calls[0][0]
    expect(completion.path).toEqual({ uploadId: session.uploadId })
    expect(completion.headers).toEqual({ 'Idempotency-Key': expect.any(String), 'If-Match': '"rev-1"' })
    expect(listPlatformImages).toHaveBeenCalledTimes(1)
  })

  it('reuses a pending creation idempotency key only for the same file and body', async () => {
    const session = {
      uploadId: '0197f0e0-0000-7000-8000-000000000003',
      kind: 'container' as const,
      binding: 'ubuntu-24.04-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      archiveBytes: 7,
      archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
      uploadTarget: {
        partSizeBytes: 64 * 1024 * 1024,
        parts: [{ partNumber: 1, uploadUrl: 'https://objects.example.test/staged-archive', requiredHeaders: {}, expiresAt: '2026-07-16T09:00:00.000Z' }],
        expiresAt: '2026-07-16T09:00:00.000Z',
      },
      uploadedParts: [],
      expiresAt: '2026-07-16T09:00:00.000Z',
      revision: 1,
    }
    vi.mocked(createPlatformImageUpload)
      .mockResolvedValueOnce({ error: new Error('request timed out') } as never)
      .mockResolvedValueOnce({ data: session, error: undefined as never })
    vi.mocked(completePlatformImageUpload).mockResolvedValue({ data: entry, error: undefined as never })
    vi.mocked(getPlatformImageUpload).mockResolvedValue({
      data: { uploadId: session.uploadId, revision: 2, state: 'imported', catalogId: entry.catalogId },
      error: undefined as never,
    })
    const input = {
      kind: 'container' as const,
      binding: 'ubuntu-24.04-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 3,
      reason: '导入已评审归档',
    }
    const file = new File(['archive'], 'layout.tar', { type: 'application/vnd.oci.image.layout.v1+tar' })
    const images = usePlatformImages()

    await expect(images.upload(file, input)).resolves.toBe(false)
    const firstKey = vi.mocked(createPlatformImageUpload).mock.calls[0][0].headers['Idempotency-Key']
    expect(window.sessionStorage.getItem('labweaver.platform-image-upload:create')).toContain(firstKey)

    await expect(images.upload(new File(['archive'], 'different-name.tar'), input)).resolves.toBe(false)
    expect(createPlatformImageUpload).toHaveBeenCalledOnce()
    expect(images.state).toMatchObject({ kind: 'error', diagnostic: { code: 'PLATFORM_IMAGE_UPLOAD_CREATION_PENDING' } })

    await expect(images.upload(file, input)).resolves.toBe(true)
    expect(createPlatformImageUpload).toHaveBeenCalledTimes(2)
    expect(vi.mocked(createPlatformImageUpload).mock.calls[1][0].headers['Idempotency-Key']).toBe(firstKey)
    expect(window.sessionStorage.getItem('labweaver.platform-image-upload:create')).toBeNull()
  })

  it('clears a creation intent after a deterministic client rejection', async () => {
    vi.mocked(createPlatformImageUpload)
      .mockResolvedValueOnce({
        error: problem('LW_PLATFORM_IMAGE_UPLOAD_INVALID', '���ϴ������Ч��'),
        response: { status: 422 },
      } as never)
      .mockResolvedValueOnce({
        error: problem('LW_PLATFORM_IMAGE_UPLOAD_INVALID', '�������Ȼ����Ч��'),
        response: { status: 422 },
      } as never)
    const file = new File(['archive'], 'layout.tar', { type: 'application/vnd.oci.image.layout.v1+tar' })
    const input = {
      kind: 'container' as const,
      binding: 'ubuntu-24.04-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 3,
      reason: '�������һ������',
    }
    const images = usePlatformImages()

    await expect(images.upload(file, input)).resolves.toBe(false)
    expect(window.sessionStorage.getItem('labweaver.platform-image-upload:create')).toBeNull()

    await expect(images.upload(file, { ...input, reason: '�޸�������' })).resolves.toBe(false)
    expect(createPlatformImageUpload).toHaveBeenCalledTimes(2)
    expect(createPlatformImageUpload.mock.calls[1][0].headers['Idempotency-Key'])
      .not.toBe(createPlatformImageUpload.mock.calls[0][0].headers['Idempotency-Key'])
    expect(images.state).toMatchObject({ kind: 'error', diagnostic: { code: 'LW_PLATFORM_IMAGE_UPLOAD_INVALID' } })
  })

  it('keeps the creation key when the server reports an in-progress operation', async () => {
    vi.mocked(createPlatformImageUpload)
      .mockResolvedValueOnce({
        error: {
          diagnosticCode: 'LW_OPERATION_IN_PROGRESS',
          detail: '�����ϴ�����������',
          retryable: true,
          status: 409,
        },
        response: { status: 409 },
      } as never)
      .mockResolvedValueOnce({ error: new Error('request timed out') } as never)
    const input = {
      kind: 'container' as const,
      binding: 'ubuntu-24.04-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 3,
      reason: '�������ϴ�',
    }
    const file = new File(['archive'], 'layout.tar', { type: 'application/vnd.oci.image.layout.v1+tar' })
    const images = usePlatformImages()

    await expect(images.upload(file, input)).resolves.toBe(false)
    const firstKey = createPlatformImageUpload.mock.calls[0][0].headers['Idempotency-Key']
    expect(window.sessionStorage.getItem('labweaver.platform-image-upload:create')).toContain(firstKey)

    await expect(images.upload(file, input)).resolves.toBe(false)
    expect(createPlatformImageUpload).toHaveBeenCalledTimes(2)
    expect(createPlatformImageUpload.mock.calls[1][0].headers['Idempotency-Key']).toBe(firstKey)
    expect(window.sessionStorage.getItem('labweaver.platform-image-upload:create')).toContain(firstKey)
  })

  it('uploads at most three missing parts and completes with the server manifest', async () => {
    const partSize = 64 * 1024 * 1024
    const uploadId = '0197f0e0-0000-7000-8000-000000000040'
    const archiveBytes = partSize * 4 + 1
    const parts = Array.from({ length: 5 }, (_, index) => ({
      partNumber: index + 1,
      uploadUrl: `https://objects.example.test/part-${index + 1}`,
      requiredHeaders: {},
      expiresAt: '2026-07-16T09:00:00.000Z',
    }))
    vi.mocked(createPlatformImageUpload).mockResolvedValue({
      data: {
        uploadId,
        kind: 'container' as const,
        binding: 'ubuntu-24.04-v1',
        targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
        archiveBytes,
        archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
        uploadTarget: { partSizeBytes: partSize, parts, expiresAt: '2026-07-16T09:00:00.000Z' },
        uploadedParts: [],
        expiresAt: '2026-07-16T09:00:00.000Z',
        revision: 1,
      },
      error: undefined as never,
    })
    vi.mocked(completePlatformImageUpload).mockResolvedValue({ data: entry, error: undefined as never })
    vi.mocked(getPlatformImageUpload).mockResolvedValue({
      data: { uploadId, revision: 2, state: 'imported', catalogId: entry.catalogId },
      error: undefined as never,
    })
    const file = {
      size: archiveBytes,
      slice: vi.fn(() => new Blob(['part'])),
    } as unknown as File
    let inFlight = 0
    let maxInFlight = 0
    const releases: Array<() => void> = []
    vi.mocked(putFileWithProgress).mockImplementation(async (_file, _url, _headers, onProgress) => {
      inFlight += 1
      maxInFlight = Math.max(maxInFlight, inFlight)
      onProgress(100)
      await new Promise<void>((resolve) => releases.push(resolve))
      inFlight -= 1
      return { etag: `"etag-${vi.mocked(putFileWithProgress).mock.calls.length}"` }
    })
    const images = usePlatformImages()
    const uploadPromise = images.upload(file, {
      kind: 'container',
      binding: 'ubuntu-24.04-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 3,
      reason: '导入已评审归档',
    })

    await vi.waitFor(() => expect(putFileWithProgress).toHaveBeenCalledTimes(3))
    expect(maxInFlight).toBe(3)
    while (putFileWithProgress.mock.calls.length < 5) {
      releases.shift()?.()
      await Promise.resolve()
    }
    while (releases.length > 0) {
      releases.shift()?.()
      await Promise.resolve()
    }
    await expect(uploadPromise).resolves.toBe(true)

    expect(completePlatformImageUpload.mock.calls[0][0].body).toEqual({
      parts: [1, 2, 3, 4, 5].map((partNumber) => ({ partNumber, etag: expect.any(String) })),
    })
  })

  it('reselects the same file and uploads only parts still missing after refresh', async () => {
    const partSize = 64 * 1024 * 1024
    const uploadId = '0197f0e0-0000-7000-8000-000000000041'
    const archiveBytes = partSize + 2
    const uploadedPart = { partNumber: 1, etag: '"already-there"', sizeBytes: partSize }
    const target = {
      partSizeBytes: partSize,
      parts: [{ partNumber: 2, uploadUrl: 'https://objects.example.test/part-2', requiredHeaders: {}, expiresAt: '2026-07-16T09:00:00.000Z' }],
      expiresAt: '2026-07-16T09:00:00.000Z',
    }
    window.sessionStorage.setItem('labweaver.platform-image-upload', JSON.stringify({
      uploadId,
      revision: 1,
      completeIdempotencyKey: 'complete-key',
      cancelIdempotencyKey: 'cancel-key',
      phase: 'uploading',
      state: 'pending',
      archiveBytes,
    }))
    vi.mocked(getPlatformImageUpload)
      .mockResolvedValueOnce({
        data: { uploadId, revision: 1, state: 'pending', uploadTarget: target, uploadedParts: [uploadedPart] },
        error: undefined as never,
      })
      .mockResolvedValueOnce({
        data: { uploadId, revision: 1, state: 'pending', uploadTarget: target, uploadedParts: [uploadedPart] },
        error: undefined as never,
      })
      .mockResolvedValueOnce({ data: { uploadId, revision: 2, state: 'imported', catalogId: entry.catalogId }, error: undefined as never })
    vi.mocked(completePlatformImageUpload).mockResolvedValue({ data: entry, error: undefined as never })
    vi.mocked(putFileWithProgress).mockResolvedValue({ etag: '"new-part"' })
    const file = {
      size: archiveBytes,
      slice: vi.fn(() => new Blob(['part'])),
    } as unknown as File
    const images = usePlatformImages()

    await expect(images.resumeUpload()).resolves.toBe(false)
    expect(images.uploadNeedsFile).toBe(true)
    await expect(images.upload(file, {
      kind: 'container',
      binding: 'ubuntu-24.04-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 3,
      reason: '继续导入已评审归档',
    })).resolves.toBe(true)

    expect(putFileWithProgress).toHaveBeenCalledOnce()
    expect(putFileWithProgress.mock.calls[0][1]).toBe(target.parts[0].uploadUrl)
    expect(completePlatformImageUpload.mock.calls[0][0].body).toEqual({
      parts: [
        { partNumber: 1, etag: uploadedPart.etag },
        { partNumber: 2, etag: '"new-part"' },
      ],
    })
  })

  it('reports a failed object upload with the upload diagnostic', async () => {
    vi.mocked(createPlatformImageUpload).mockResolvedValue({
      data: {
        uploadId: '0197f0e0-0000-7000-8000-000000000002',
        kind: 'container' as const,
        binding: 'ubuntu-24.04-v1',
        targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
        archiveBytes: 7,
        archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
        uploadTarget: { partSizeBytes: 64 * 1024 * 1024, parts: [{ partNumber: 1, uploadUrl: 'https://objects.example.test/staged-archive', requiredHeaders: {}, expiresAt: '2026-07-16T09:00:00.000Z' }], expiresAt: '2026-07-16T09:00:00.000Z' }, uploadedParts: [],
        expiresAt: '2026-07-16T09:00:00.000Z',
        revision: 1,
      },
      error: undefined as never,
    })
    const uploadError = '对象存储上传失败：HTTP 403；对象存储错误 AccessDenied：Forbidden'
    vi.mocked(putFileWithProgress).mockRejectedValue(new Error(uploadError))
    const images = usePlatformImages()

    await expect(images.upload(new File(['archive'], 'layout.tar'), {
      kind: 'container',
      binding: 'ubuntu-24.04-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 3,
      reason: '导入已评审归档',
    })).resolves.toBe(false)

    expect(completePlatformImageUpload).not.toHaveBeenCalled()
    expect(images.state.kind).toBe('error')
    if (images.state.kind === 'error') {
      expect(images.state.diagnostic.code).toBe('PLATFORM_IMAGE_UPLOAD_FAILED')
      expect(images.state.diagnostic.message).toBe(uploadError)
      expect(images.state.diagnostic.retryable).toBe(false)
    }
  })

  it('aborts an in-flight archive PUT when the upload session expires', async () => {
    vi.useFakeTimers()
    try {
      const uploadId = '0197f0e0-0000-7000-8000-000000000030'
      vi.mocked(createPlatformImageUpload).mockResolvedValue({
        data: {
          uploadId,
          kind: 'virtual_machine' as const,
          binding: 'ubuntu-24.04-vm-v1',
          targetReference: 'harbor.lab.lan/labweaver-system/ubuntu-vm:24.04',
          archiveBytes: 7,
          archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
          uploadTarget: { partSizeBytes: 64 * 1024 * 1024, parts: [{ partNumber: 1, uploadUrl: 'https://objects.example.test/staged-template', requiredHeaders: {}, expiresAt: '2026-10-09T04:00:00.000Z' }], expiresAt: '2026-10-09T04:00:00.000Z' }, uploadedParts: [],
          expiresAt: '2026-10-09T04:00:00.000Z',
          revision: 1,
        },
        error: undefined as never,
      })
      vi.mocked(getPlatformImageUpload).mockResolvedValue({
        data: { uploadId, revision: 2, state: 'failed', diagnostic: 'LW_PLATFORM_IMAGE_UPLOAD_EXPIRED' },
        error: undefined as never,
      })
      vi.mocked(putFileWithProgress).mockImplementation(async (_file, _url, _headers, _onProgress, signal) => {
        await new Promise<void>((_, reject) => {
          signal?.addEventListener('abort', () => reject(new Error('上传已取消。')), { once: true })
        })
      })
      const images = usePlatformImages()
      const uploadPromise = images.upload(new File(['archive'], 'template.tar'), {
        kind: 'virtual_machine',
        binding: 'ubuntu-24.04-vm-v1',
        targetReference: 'harbor.lab.lan/labweaver-system/ubuntu-vm:24.04',
        trustRevision: 1,
        reason: '导入已评审虚拟机模板',
        diskFormat: 'qcow2',
        diskPath: 'disk/disk.img',
        capacityBytes: 10737418240,
      })

      await vi.advanceTimersByTimeAsync(1000)
      await expect(uploadPromise).resolves.toBe(false)
      expect(completePlatformImageUpload).not.toHaveBeenCalled()
      expect(images.state).toMatchObject({
        kind: 'terminal',
        uploadId,
        state: 'failed',
        diagnostic: {
          code: 'LW_PLATFORM_IMAGE_UPLOAD_EXPIRED',
          message: '上传会话已过期，请重新选择归档文件上传。',
          retryable: false,
        },
      })
      expect(window.sessionStorage.getItem('labweaver.platform-image-upload')).toBeNull()
    } finally {
      vi.useRealTimers()
    }
  })

  it('keeps a terminal expiry when the PUT resolves successfully on the abort tick', async () => {
    vi.useFakeTimers()
    try {
      const uploadId = '0197f0e0-0000-7000-8000-000000000033'
      vi.mocked(createPlatformImageUpload).mockResolvedValue({
        data: {
          uploadId,
          kind: 'virtual_machine' as const,
          binding: 'ubuntu-24.04-vm-v1',
          targetReference: 'harbor.lab.lan/labweaver-system/ubuntu-vm:24.04',
          archiveBytes: 7,
          archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
          uploadTarget: { partSizeBytes: 64 * 1024 * 1024, parts: [{ partNumber: 1, uploadUrl: 'https://objects.example.test/staged-template', requiredHeaders: {}, expiresAt: '2026-10-09T04:00:00.000Z' }], expiresAt: '2026-10-09T04:00:00.000Z' }, uploadedParts: [],
          expiresAt: '2026-10-09T04:00:00.000Z',
          revision: 1,
        },
        error: undefined as never,
      })
      let resolvePut!: () => void
      vi.mocked(putFileWithProgress).mockImplementation(async (_file, _url, _headers, _onProgress, signal) => {
        await new Promise<void>((resolve) => {
          resolvePut = resolve
          signal?.addEventListener('abort', resolve, { once: true })
        })
      })
      let resolveStatus!: (result: Awaited<ReturnType<typeof getPlatformImageUpload>>) => void
      vi.mocked(getPlatformImageUpload).mockReturnValue(new Promise((resolve) => {
        resolveStatus = resolve
      }) as ReturnType<typeof getPlatformImageUpload>)
      const images = usePlatformImages()
      const uploadPromise = images.upload(new File(['archive'], 'template.tar'), {
        kind: 'virtual_machine',
        binding: 'ubuntu-24.04-vm-v1',
        targetReference: 'harbor.lab.lan/labweaver-system/ubuntu-vm:24.04',
        trustRevision: 1,
        reason: '导入已评审虚拟机模板',
        diskFormat: 'qcow2',
        diskPath: 'disk/disk.img',
        capacityBytes: 10737418240,
      })

      await vi.advanceTimersByTimeAsync(1000)
      expect(getPlatformImageUpload).toHaveBeenCalledOnce()
      resolveStatus({
        data: { uploadId, revision: 2, state: 'failed', diagnostic: 'LW_PLATFORM_IMAGE_UPLOAD_EXPIRED' },
        error: undefined as never,
      })
      await vi.advanceTimersByTimeAsync(0)
      expect(resolvePut).toBeDefined()
      await expect(uploadPromise).resolves.toBe(false)
      expect(completePlatformImageUpload).not.toHaveBeenCalled()
      expect(images.state).toMatchObject({
        kind: 'terminal',
        uploadId,
        state: 'failed',
        diagnostic: { code: 'LW_PLATFORM_IMAGE_UPLOAD_EXPIRED' },
      })
    } finally {
      vi.useRealTimers()
    }
  })

  it('does not let the transfer monitor replace a user cancellation race', async () => {
    vi.useFakeTimers()
    try {
      const uploadId = '0197f0e0-0000-7000-8000-000000000031'
      vi.mocked(createPlatformImageUpload).mockResolvedValue({
        data: {
          uploadId,
          kind: 'container' as const,
          binding: 'ubuntu-24.04-v1',
          targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
          archiveBytes: 7,
          archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
          uploadTarget: { partSizeBytes: 64 * 1024 * 1024, parts: [{ partNumber: 1, uploadUrl: 'https://objects.example.test/staged-archive', requiredHeaders: {}, expiresAt: '2026-10-09T04:00:00.000Z' }], expiresAt: '2026-10-09T04:00:00.000Z' }, uploadedParts: [],
          expiresAt: '2026-10-09T04:00:00.000Z',
          revision: 1,
        },
        error: undefined as never,
      })
      vi.mocked(cancelPlatformImageUpload).mockResolvedValue({ data: {}, error: undefined as never })
      vi.mocked(getPlatformImageUpload).mockResolvedValue({
        data: { uploadId, revision: 2, state: 'cancelled' },
        error: undefined as never,
      })
      vi.mocked(putFileWithProgress).mockImplementation(async (_file, _url, _headers, _onProgress, signal) => {
        await new Promise<void>((_, reject) => {
          signal?.addEventListener('abort', () => reject(new Error('上传已取消。')), { once: true })
        })
      })
      const images = usePlatformImages()
      const uploadPromise = images.upload(new File(['archive'], 'layout.tar'), {
        kind: 'container',
        binding: 'ubuntu-24.04-v1',
        targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
        trustRevision: 3,
        reason: '导入已评审归档',
      })

      await vi.advanceTimersByTimeAsync(0)
      await expect(images.cancelUpload()).resolves.toBe(true)
      await expect(uploadPromise).resolves.toBe(false)
      expect(images.state).toMatchObject({ kind: 'terminal', uploadId, state: 'cancelled' })
      expect(getPlatformImageUpload).toHaveBeenCalledOnce()
      await vi.advanceTimersByTimeAsync(2000)
      expect(getPlatformImageUpload).toHaveBeenCalledOnce()
    } finally {
      vi.useRealTimers()
    }
  })

  it('clears transfer status polling when the owner unmounts', async () => {
    vi.useFakeTimers()
    try {
      const uploadId = '0197f0e0-0000-7000-8000-000000000032'
      vi.mocked(createPlatformImageUpload).mockResolvedValue({
        data: {
          uploadId,
          kind: 'container' as const,
          binding: 'ubuntu-24.04-v1',
          targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
          archiveBytes: 7,
          archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
          uploadTarget: { partSizeBytes: 64 * 1024 * 1024, parts: [{ partNumber: 1, uploadUrl: 'https://objects.example.test/staged-archive', requiredHeaders: {}, expiresAt: '2026-10-09T04:00:00.000Z' }], expiresAt: '2026-10-09T04:00:00.000Z' }, uploadedParts: [],
          expiresAt: '2026-10-09T04:00:00.000Z',
          revision: 1,
        },
        error: undefined as never,
      })
      vi.mocked(putFileWithProgress).mockImplementation(async (_file, _url, _headers, _onProgress, signal) => {
        await new Promise<void>((_, reject) => {
          signal?.addEventListener('abort', () => reject(new Error('上传已取消。')), { once: true })
        })
      })
      let images!: ReturnType<typeof usePlatformImages>
      const wrapper = mount(defineComponent({
        setup() {
          images = usePlatformImages()
          return () => h('div')
        },
      }))
      const uploadPromise = images.upload(new File(['archive'], 'layout.tar'), {
        kind: 'container',
        binding: 'ubuntu-24.04-v1',
        targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
        trustRevision: 3,
        reason: '导入已评审归档',
      })

      await vi.advanceTimersByTimeAsync(0)
      wrapper.unmount()
      await expect(uploadPromise).resolves.toBe(false)
      await vi.advanceTimersByTimeAsync(2000)
      expect(getPlatformImageUpload).not.toHaveBeenCalled()
    } finally {
      vi.useRealTimers()
    }
  })

  it('clears an expired upload and completes only after the user selects a new file', async () => {
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
      .mockResolvedValueOnce({
        data: { uploadId: replacementUploadId, revision: 2, state: 'imported', catalogId: entry.catalogId },
        error: undefined as never,
      })
    vi.mocked(createPlatformImageUpload).mockResolvedValue({
      data: {
        uploadId: replacementUploadId,
        kind: 'container' as const,
        binding: 'ubuntu-24.04-v1',
        targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
        archiveBytes: 13,
        archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
        uploadTarget: { partSizeBytes: 64 * 1024 * 1024, parts: [{ partNumber: 1, uploadUrl: 'https://objects.example.test/replacement', requiredHeaders: {}, expiresAt: '2026-10-01T00:00:00.000Z'  }], expiresAt: '2026-10-01T00:00:00.000Z'  }, uploadedParts: [],
        expiresAt: '2026-10-01T00:00:00.000Z',
        revision: 1,
      },
      error: undefined as never,
    })
    vi.mocked(putFileWithProgress).mockResolvedValue({ etag: '"etag-1"' })
    vi.mocked(completePlatformImageUpload).mockResolvedValue({ data: entry, error: undefined as never })
    const images = usePlatformImages()

    await expect(images.resumeUpload()).resolves.toBe(false)
    expect(images.state).toMatchObject({
      kind: 'terminal',
      uploadId: expiredUploadId,
      state: 'failed',
      diagnostic: {
        code: 'LW_PLATFORM_IMAGE_UPLOAD_EXPIRED',
        message: '上传会话已过期，请重新选择归档文件上传。',
        retryable: false,
      },
    })
    expect(images.uploadActive).toBe(false)
    expect(window.sessionStorage.getItem('labweaver.platform-image-upload')).toBeNull()
    expect(completePlatformImageUpload).not.toHaveBeenCalled()

    await expect(images.upload(new File(['fresh archive'], 'layout.tar'), {
      kind: 'container',
      binding: 'ubuntu-24.04-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 3,
      reason: '重新上传已过期归档',
    })).resolves.toBe(true)

    expect(completePlatformImageUpload).toHaveBeenCalledOnce()
    expect(completePlatformImageUpload.mock.calls[0][0].path).toEqual({ uploadId: replacementUploadId })
    expect(getPlatformImageUpload.mock.calls).toEqual([
      [{ path: { uploadId: expiredUploadId } }],
      [{ path: { uploadId: replacementUploadId } }],
    ])
  })

  it('submits the base-disk descriptor for a virtual-machine upload', async () => {
    vi.mocked(createPlatformImageUpload).mockResolvedValue({
      data: {
        uploadId: '0197f0e0-0000-7000-8000-000000000005',
        kind: 'virtual_machine' as const,
        binding: 'ubuntu-24.04-vm-v1',
        targetReference: 'harbor.lab.lan/labweaver-system/ubuntu-vm:24.04',
        archiveBytes: 4,
        archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
        capacityBytes: 10737418240,
        diskFormat: 'qcow2' as const,
        diskPath: 'disk/disk.img',
        uploadTarget: { partSizeBytes: 64 * 1024 * 1024, parts: [{ partNumber: 1, uploadUrl: 'https://objects.example.test/staged-template', requiredHeaders: { 'x-amz-server-side-encryption': 'AES256' }, expiresAt: '2026-07-16T09:00:00.000Z' }], expiresAt: '2026-07-16T09:00:00.000Z' }, uploadedParts: [],
        expiresAt: '2026-07-16T09:00:00.000Z',
        revision: 1,
      },
      error: undefined as never,
    })
    vi.mocked(completePlatformImageUpload).mockResolvedValue({ data: entry, error: undefined as never })
    vi.mocked(getPlatformImageUpload).mockResolvedValue({
      data: { uploadId: '0197f0e0-0000-7000-8000-000000000005', revision: 2, state: 'imported', catalogId: entry.catalogId },
      error: undefined as never,
    })
    vi.mocked(putFileWithProgress).mockResolvedValue({ etag: '"etag-1"' })
    const images = usePlatformImages()

    await expect(images.upload(new File(['disk'], 'template.tar'), {
      kind: 'virtual_machine',
      binding: 'ubuntu-24.04-vm-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu-vm:24.04',
      trustRevision: 2,
      reason: '导入已评审虚拟机模板',
      diskFormat: 'qcow2',
      diskPath: 'disk/disk.img',
      capacityBytes: 10737418240,
    })).resolves.toBe(true)

    expect(vi.mocked(createPlatformImageUpload).mock.calls[0][0].body).toEqual({
      kind: 'virtual_machine',
      binding: 'ubuntu-24.04-vm-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu-vm:24.04',
      trustRevision: 2,
      reason: '导入已评审虚拟机模板',
      archiveBytes: 4,
      archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
      diskFormat: 'qcow2',
      diskPath: 'disk/disk.img',
      capacityBytes: 10737418240,
    })
    expect(listPlatformImages).toHaveBeenCalledTimes(1)
  })

  it('resumes a submitted import from session storage and clears it after import', async () => {
    window.sessionStorage.setItem('labweaver.platform-image-upload', JSON.stringify({
      uploadId: '0197f0e0-0000-7000-8000-000000000006',
      revision: 2,
      completeIdempotencyKey: 'complete-key',
      cancelIdempotencyKey: 'cancel-key',
      phase: 'completing',
      state: 'importing',
      archiveBytes: 7,
    }))
    vi.mocked(getPlatformImageUpload).mockResolvedValue({
      data: {
        uploadId: '0197f0e0-0000-7000-8000-000000000006',
        revision: 3,
        state: 'imported',
        catalogId: entry.catalogId,
      },
      error: undefined as never,
    })
    const images = usePlatformImages()

    await expect(images.resumeUpload()).resolves.toBe(true)

    expect(images.state).toMatchObject({ kind: 'terminal', state: 'imported' })
    expect(window.sessionStorage.getItem('labweaver.platform-image-upload')).toBeNull()
  })

  it('clears a persisted session when the server reports it missing', async () => {
    const uploadId = '0197f0e0-0000-7000-8000-000000000006'
    window.sessionStorage.setItem('labweaver.platform-image-upload', JSON.stringify({
      uploadId,
      revision: 2,
      completeIdempotencyKey: 'complete-key',
      cancelIdempotencyKey: 'cancel-key',
      phase: 'uploading',
      state: 'pending',
      archiveBytes: 7,
    }))
    vi.mocked(getPlatformImageUpload).mockResolvedValue({
      error: problem('LW_PLATFORM_IMAGE_UPLOAD_NOT_FOUND', '上传会话不存在。'),
      response: { status: 404 },
    } as never)
    const images = usePlatformImages()

    await expect(images.resumeUpload()).resolves.toBe(false)

    expect(images.uploadActive).toBe(false)
    expect(window.sessionStorage.getItem('labweaver.platform-image-upload')).toBeNull()
    expect(images.state).toMatchObject({
      kind: 'error',
      uploadId,
      diagnostic: { code: 'LW_PLATFORM_IMAGE_UPLOAD_NOT_FOUND', retryable: false },
    })
  })

  it('does not poll an interrupted object upload until the file is selected again or cancelled', async () => {
    window.sessionStorage.setItem('labweaver.platform-image-upload', JSON.stringify({
      uploadId: '0197f0e0-0000-7000-8000-000000000007',
      revision: 2,
      completeIdempotencyKey: 'complete-key',
      cancelIdempotencyKey: 'cancel-key',
      phase: 'uploading',
      state: 'pending',
      archiveBytes: 7,
    }))
    vi.mocked(getPlatformImageUpload).mockResolvedValue({
      data: {
        uploadId: '0197f0e0-0000-7000-8000-000000000007',
        revision: 2,
        state: 'pending',
      },
      error: undefined as never,
    })
    const images = usePlatformImages()

    await expect(images.resumeUpload()).resolves.toBe(false)

    expect(images.state).toMatchObject({ kind: 'error', diagnostic: { code: 'PLATFORM_IMAGE_UPLOAD_FILE_REQUIRED' } })
    if (images.state.kind === 'error') expect(images.state.diagnostic.retryable).toBe(false)
    expect(images.uploadActive).toBe(true)
  })

  it('keeps cancellation pending when status is unavailable and reconciles it on refresh', async () => {
    const uploadId = '0197f0e0-0000-7000-8000-000000000010'
    window.sessionStorage.setItem('labweaver.platform-image-upload', JSON.stringify({
      uploadId,
      revision: 2,
      completeIdempotencyKey: 'complete-key',
      cancelIdempotencyKey: 'cancel-key',
      phase: 'uploading',
      state: 'pending',
      archiveBytes: 7,
    }))
    vi.mocked(getPlatformImageUpload)
      .mockResolvedValueOnce({
        data: { uploadId, revision: 2, state: 'pending' },
        error: undefined as never,
      })
      .mockResolvedValueOnce({ error: new Error('status unavailable') } as never)
      .mockResolvedValueOnce({
        data: { uploadId, revision: 3, state: 'cancelled' },
        error: undefined as never,
      })
    vi.mocked(cancelPlatformImageUpload).mockResolvedValue({
      error: problem('LW_PLATFORM_IMAGE_UPLOAD_CANCEL_FAILED', '取消请求暂时无法确认。'),
    } as never)
    const images = usePlatformImages()

    await images.resumeUpload()
    await expect(images.cancelUpload()).resolves.toBe(false)

    expect(images.uploadCancellationPending).toBe(true)
    expect(images.state).toMatchObject({
      kind: 'error',
      uploadId,
      diagnostic: { code: 'LW_PLATFORM_IMAGE_UPLOAD_CANCEL_FAILED' },
    })
    expect(JSON.parse(window.sessionStorage.getItem('labweaver.platform-image-upload')!)).toMatchObject({
      uploadId,
      state: 'cancelling',
    })

    await images.resumeUpload()

    expect(images.state).toMatchObject({ kind: 'terminal', state: 'cancelled', uploadId })
    expect(images.uploadActive).toBe(false)
    expect(window.sessionStorage.getItem('labweaver.platform-image-upload')).toBeNull()
  })

  it('cancels an interrupted upload with its persisted revision fence and key', async () => {
    window.sessionStorage.setItem('labweaver.platform-image-upload', JSON.stringify({
      uploadId: '0197f0e0-0000-7000-8000-000000000008',
      revision: 2,
      completeIdempotencyKey: 'complete-key',
      cancelIdempotencyKey: 'cancel-key',
      phase: 'uploading',
      state: 'pending',
      archiveBytes: 7,
    }))
    vi.mocked(getPlatformImageUpload)
      .mockResolvedValueOnce({
        data: {
          uploadId: '0197f0e0-0000-7000-8000-000000000008',
          revision: 2,
          state: 'pending',
        },
        error: undefined as never,
      })
      .mockResolvedValueOnce({
        data: {
          uploadId: '0197f0e0-0000-7000-8000-000000000008',
          revision: 3,
          state: 'cancelled',
        },
        error: undefined as never,
      })
    vi.mocked(cancelPlatformImageUpload).mockResolvedValue({ data: {}, error: undefined as never })
    const images = usePlatformImages()
    await images.resumeUpload()

    await expect(images.cancelUpload()).resolves.toBe(true)

    expect(cancelPlatformImageUpload).toHaveBeenCalledWith(expect.objectContaining({
      path: { uploadId: '0197f0e0-0000-7000-8000-000000000008' },
      headers: { 'Idempotency-Key': 'cancel-key', 'If-Match': '"rev-2"' },
      body: { expectedRevision: 2 },
    }))
    expect(images.state).toMatchObject({ kind: 'terminal', state: 'cancelled' })
    expect(window.sessionStorage.getItem('labweaver.platform-image-upload')).toBeNull()
  })

  it('keeps cancellation terminal when it wins a completion race', async () => {
    const uploadId = '0197f0e0-0000-7000-8000-000000000009'
    vi.mocked(createPlatformImageUpload).mockResolvedValue({
      data: {
        uploadId,
        kind: 'container' as const,
        binding: 'ubuntu-24.04-v1',
        targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
        archiveBytes: 7,
        archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
        uploadTarget: { partSizeBytes: 64 * 1024 * 1024, parts: [{ partNumber: 1, uploadUrl: 'https://objects.example.test/staged-archive', requiredHeaders: {}, expiresAt: '2026-07-16T09:00:00.000Z' }], expiresAt: '2026-07-16T09:00:00.000Z' }, uploadedParts: [],
        expiresAt: '2026-07-16T09:00:00.000Z',
        revision: 1,
      },
      error: undefined as never,
    })
    vi.mocked(putFileWithProgress).mockResolvedValue({ etag: '"etag-1"' })
    let resolveCompletion!: (value: unknown) => void
    const completion = new Promise<unknown>((resolve) => { resolveCompletion = resolve })
    vi.mocked(completePlatformImageUpload).mockReturnValue(completion as never)
    vi.mocked(cancelPlatformImageUpload).mockResolvedValue({ data: {}, error: undefined as never })
    vi.mocked(getPlatformImageUpload).mockResolvedValue({
      data: { uploadId, revision: 2, state: 'cancelled' },
      error: undefined as never,
    })
    const images = usePlatformImages()
    const uploadPromise = images.upload(new File(['archive'], 'layout.tar'), {
      kind: 'container',
      binding: 'ubuntu-24.04-v1',
      targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
      trustRevision: 3,
      reason: '导入已评审归档',
    })

    await vi.waitFor(() => expect(completePlatformImageUpload).toHaveBeenCalledOnce())
    await expect(images.cancelUpload()).resolves.toBe(true)
    expect(images.state).toMatchObject({ kind: 'terminal', state: 'cancelled' })
    expect(images.state).toMatchObject({
      kind: 'terminal',
      diagnostic: { code: 'LW_PLATFORM_IMAGE_UPLOAD_CANCELLED', message: '镜像导入已取消。', retryable: false },
    })

    resolveCompletion({ data: entry, error: undefined as never })
    await expect(uploadPromise).resolves.toBe(false)
    expect(getPlatformImageUpload).toHaveBeenCalledOnce()
  })
})
