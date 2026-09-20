import { describe, it, expect, vi, beforeEach } from 'vitest'
import { usePlatformImages } from '@/composables/usePlatformImages'
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

function problem(diagnosticCode: string, detail: string) {
  return { diagnosticCode, detail, retryable: false }
}

describe('usePlatformImages', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    vi.mocked(listPlatformImages).mockResolvedValue({ data: { entries: [entry] }, error: undefined as never })
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
      archiveBytes: 2048,
      archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
      uploadTarget: {
        uploadUrl: 'https://objects.example.test/staged-archive',
        requiredHeaders: { 'x-amz-server-side-encryption': 'AES256' },
        expiresAt: '2026-07-16T09:00:00.000Z',
      },
      expiresAt: '2026-07-16T09:00:00.000Z',
      revision: 1,
    }
    vi.mocked(createPlatformImageUpload).mockResolvedValue({ data: session, error: undefined as never })
    vi.mocked(completePlatformImageUpload).mockResolvedValue({ data: entry, error: undefined as never })
    const images = usePlatformImages()
    let observedProgress: number | null = null
    vi.mocked(putFileWithProgress).mockImplementation(async (_file, _url, _headers, onProgress) => {
      onProgress(42)
      observedProgress = images.state.kind === 'uploading' ? images.state.progress : null
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
    expect(putFileWithProgress).toHaveBeenCalledWith(
      file,
      session.uploadTarget.uploadUrl,
      session.uploadTarget.requiredHeaders,
      expect.any(Function),
    )
    expect(observedProgress).toBe(42)
    const completion = vi.mocked(completePlatformImageUpload).mock.calls[0][0]
    expect(completion.path).toEqual({ uploadId: session.uploadId })
    expect(completion.headers).toEqual({ 'Idempotency-Key': expect.any(String), 'If-Match': '"rev-1"' })
    expect(listPlatformImages).toHaveBeenCalledTimes(1)
  })

  it('reports a failed object upload with the upload diagnostic', async () => {
    vi.mocked(createPlatformImageUpload).mockResolvedValue({
      data: {
        uploadId: '0197f0e0-0000-7000-8000-000000000002',
        kind: 'container' as const,
        binding: 'ubuntu-24.04-v1',
        targetReference: 'harbor.lab.lan/labweaver-system/ubuntu:24.04',
        archiveBytes: 2048,
        archiveMediaType: 'application/vnd.oci.image.layout.v1+tar',
        uploadTarget: {
          uploadUrl: 'https://objects.example.test/staged-archive',
          requiredHeaders: {},
          expiresAt: '2026-07-16T09:00:00.000Z',
        },
        expiresAt: '2026-07-16T09:00:00.000Z',
        revision: 1,
      },
      error: undefined as never,
    })
    vi.mocked(putFileWithProgress).mockRejectedValue(new Error('上传失败：403 Forbidden'))
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
      expect(images.state.diagnostic.message).toBe('上传并导入 OCI 归档失败。')
      expect(images.state.diagnostic.retryable).toBe(true)
    }
  })
})
