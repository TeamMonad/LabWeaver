import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { ref } from 'vue'
import { useProjectProblemPackageUpload } from '@/composables/useProjectProblemPackageUpload'
import { createProjectProblemPackageUpload } from '@/generated/contracts'

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return {
    ...actual,
    createProjectProblemPackageUpload: vi.fn(),
    completeProjectProblemPackageUpload: vi.fn(),
  }
})

vi.mock('@/utils/crypto', () => ({
  sha256File: vi.fn(async (file: File) => `sha256-${file.name}`),
  computeManifestSha256: vi.fn(async () => 'manifest-sha256'),
}))

function makeFile(name: string, content: string): File {
  return new File([content], name, { type: 'text/plain' })
}

class MockUploadXhr {
  status = 0
  statusText = ''
  upload = {
    addEventListener: (type: string, listener: (event: ProgressEvent) => void) => {
      if (type === 'progress') this.progressListener = listener
    },
  }
  private readonly listeners = new Map<string, (event: Event) => void>()
  private progressListener: ((event: ProgressEvent) => void) | undefined
  private url = ''

  open(_method: string, url: string) {
    this.url = url
  }

  setRequestHeader(_name: string, _value: string) {}

  addEventListener(type: string, listener: (event: Event) => void) {
    this.listeners.set(type, listener)
  }

  send(_file: File) {
    queueMicrotask(() => {
      this.progressListener?.({ lengthComputable: true, loaded: 1, total: 1 } as ProgressEvent)
      this.status = this.url.includes('put-fail') ? 500 : 200
      this.statusText = this.status === 200 ? 'OK' : 'Upload failed'
      this.listeners.get('load')?.(new Event('load'))
    })
  }
}

function makeMockFileEntry(name: string): FileSystemFileEntry {
  const file = makeFile(name, `content of ${name}`)
  return {
    name,
    isDirectory: false,
    isFile: true,
    file: (success: (f: File) => void) => success(file),
  } as unknown as FileSystemFileEntry
}

function makeMockDirectoryEntry(name: string, children: FileSystemEntry[]): FileSystemDirectoryEntry {
  let remaining = [...children]
  const reader: FileSystemDirectoryReader = {
    readEntries(success: (entries: FileSystemEntry[]) => void) {
      const batch = remaining.slice(0, 2)
      remaining = remaining.slice(2)
      success(batch)
    },
  } as unknown as FileSystemDirectoryReader
  return {
    name,
    isDirectory: true,
    isFile: false,
    createReader: () => reader,
  } as unknown as FileSystemDirectoryEntry
}

function makeDataTransferItem(entry: FileSystemEntry): DataTransferItem {
  return {
    webkitGetAsEntry: () => entry,
  } as unknown as DataTransferItem
}

describe('useProjectProblemPackageUpload', () => {
  beforeEach(() => {
    vi.resetAllMocks()
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.unstubAllEnvs()
  })

  it('collects all files across multiple directory-entry batches', async () => {
    const projectId = ref<string | null>('project-1')
    const policyRevision = ref<number | undefined>(1)
    const courseId = ref<string | null | undefined>('course-1')
    const upload = useProjectProblemPackageUpload(projectId, policyRevision, courseId)

    const children = Array.from({ length: 5 }, (_, i) => makeMockFileEntry(`file${i}.txt`))
    const root = makeMockDirectoryEntry('materials', children)
    const items = [makeDataTransferItem(root)] as unknown as DataTransferItemList

    await upload.addDirectoryItems(items)

    expect(upload.files.length).toBe(5)
    expect(upload.files.map((f) => f.path).sort()).toEqual(
      Array.from({ length: 5 }, (_, i) => `materials/file${i}.txt`).sort(),
    )
  })

  it('sorts files by package path regardless of arrival order so manifest hashing stays deterministic', async () => {
    const projectId = ref<string | null>('project-1')
    const policyRevision = ref<number | undefined>(1)
    const courseId = ref<string | null | undefined>('course-1')
    const upload = useProjectProblemPackageUpload(projectId, policyRevision, courseId)

    const zed = makeFile('zeta.txt', 'z')
    Object.defineProperty(zed, 'webkitRelativePath', { value: 'materials/zeta.txt' })
    const alpha = makeFile('alpha.txt', 'a')
    Object.defineProperty(alpha, 'webkitRelativePath', { value: 'materials/alpha/alpha.txt' })
    const mid = makeFile('mid.txt', 'm')
    Object.defineProperty(mid, 'webkitRelativePath', { value: 'materials/mid.txt' })

    await upload.addFiles([zed, alpha, mid])

    expect(upload.files.map((f) => f.path)).toEqual([
      'materials/alpha/alpha.txt',
      'materials/mid.txt',
      'materials/zeta.txt',
    ])
  })

  it('marks object upload failure without throwing unhandled rejection', async () => {
    const projectId = ref<string | null>('project-1')
    const policyRevision = ref<number | undefined>(1)
    const courseId = ref<string | null | undefined>('course-1')
    const upload = useProjectProblemPackageUpload(projectId, policyRevision, courseId)

    const failingFile = makeFile('__put-fail__.txt', 'boom')
    Object.defineProperty(failingFile, 'webkitRelativePath', { value: '__put-fail__.txt' })
    const okFile = makeFile('main.py', 'ok')
    Object.defineProperty(okFile, 'webkitRelativePath', { value: 'main.py' })

    await upload.addFiles([failingFile, okFile])
    expect(upload.files.length).toBe(2)
    expect(upload.state.kind).toBe('ready')
    vi.stubGlobal('XMLHttpRequest', MockUploadXhr)

    vi.mocked(createProjectProblemPackageUpload).mockResolvedValue({
      data: {
        id: 'upload-1',
        projectId: 'project-1',
        courseId: 'course-1',
        expiresAt: '2026-07-16T10:00:00.000Z',
        revision: 1,
        files: [
          { path: '__put-fail__.txt', sizeBytes: 4, mediaType: 'text/plain' },
          { path: 'main.py', sizeBytes: 2, mediaType: 'text/plain' },
        ],
        uploadTargets: [
          {
            path: '__put-fail__.txt',
            uploadUrl: 'https://uploads.example.test/__put-fail__.txt',
            requiredHeaders: {},
            expiresAt: '2026-07-16T10:00:00.000Z',
          },
          {
            path: 'main.py',
            uploadUrl: 'https://uploads.example.test/main.py',
            requiredHeaders: {},
            expiresAt: '2026-07-16T10:00:00.000Z',
          },
        ],
      } as never,
      error: undefined as never,
    })

    await upload.createSession()

    await vi.waitFor(() => upload.state.kind === 'error')
    expect(upload.state.kind).toBe('error')
    if (upload.state.kind === 'error') {
      expect(upload.state.diagnostic.code).toBe('UPLOAD_OBJECT_FAILED')
      expect(upload.state.diagnostic.message).toContain('__put-fail__.txt')
    }
    expect(upload.files[0].status).toBe('error')
    expect(upload.files[1].status).toBe('done')
    expect(createProjectProblemPackageUpload).toHaveBeenCalledWith(expect.objectContaining({
      path: { projectId: 'project-1' },
      body: expect.objectContaining({
        projectId: 'project-1',
        courseId: 'course-1',
        retentionPolicyRevision: 1,
      }),
    }))
  })
})
