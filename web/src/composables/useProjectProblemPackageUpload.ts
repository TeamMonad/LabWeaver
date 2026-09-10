import { reactive, ref, type Ref } from 'vue'
import { completeProjectProblemPackageUpload, createProjectProblemPackageUpload } from '@/generated/contracts'
import type {
  ProblemPackageSchema,
  ProblemPackageUploadSessionSchema,
} from '@/generated/contracts'
import { sha256File } from '@/utils/crypto'
import { formatBytes, idempotencyKey, ifMatch } from '@/utils/format'
import { extractProblemDetails, makeDiagnostic } from '@/types/async'

export type UploadFile = {
  file: File
  path: string
  sizeBytes: number
  mediaType: string
  /** Local diagnostic hash shown to the teacher; the upload contract binds object metadata server-side. */
  sha256: string
  status: 'pending' | 'uploading' | 'done' | 'error'
  progress: number
  error?: string
  actions?: never
}

export type PackageUploadState =
  | { kind: 'idle' }
  | { kind: 'hashing' }
  | { kind: 'ready' }
  | { kind: 'creating' }
  | { kind: 'uploading' }
  | { kind: 'completing' }
  | { kind: 'done'; package: ProblemPackageSchema }
  | { kind: 'error'; diagnostic: ReturnType<typeof makeDiagnostic> }

/** Project-scoped package upload using the current session/target contract. */
export function useProjectProblemPackageUpload(
  projectId: Ref<string | null>,
  policyRevision: Ref<number | undefined>,
  courseId: Ref<string | null | undefined>,
) {
  const files = ref<UploadFile[]>([])
  const session = ref<ProblemPackageUploadSessionSchema | null>(null)
  const state = ref<PackageUploadState>({ kind: 'idle' })

  async function addDirectoryItems(items: DataTransferItemList | null) {
    if (!items) return
    const entries: FileSystemEntry[] = []
    for (let index = 0; index < items.length; index += 1) {
      const entry = items[index].webkitGetAsEntry()
      if (entry) entries.push(entry)
    }
    const collected: File[] = []
    await Promise.all(entries.map((entry) => collectFiles(entry, '', collected)))
    await addFiles(collected)
  }

  async function collectFiles(entry: FileSystemEntry, prefix: string, out: File[]): Promise<void> {
    const path = prefix ? `${prefix}/${entry.name}` : entry.name
    if (entry.isDirectory) {
      const reader = (entry as FileSystemDirectoryEntry).createReader()
      const children: FileSystemEntry[] = []
      for (;;) {
        const batch = await new Promise<FileSystemEntry[]>((resolve, reject) => reader.readEntries(resolve, reject))
        if (batch.length === 0) break
        children.push(...batch)
      }
      await Promise.all(children.map((child) => collectFiles(child, path, out)))
    } else if (entry.isFile) {
      const file = await new Promise<File>((resolve, reject) => (entry as FileSystemFileEntry).file(resolve, reject))
      Object.defineProperty(file, 'webkitRelativePath', { value: path })
      out.push(file)
    }
  }

  function packagePathOf(file: File): string {
    return (file as File & { webkitRelativePath?: string }).webkitRelativePath ?? file.name
  }

  async function addFiles(selected: File[]) {
    const ordered = [...selected].sort((left, right) => packagePathOf(left).localeCompare(packagePathOf(right)))
    state.value = { kind: 'hashing' }
    try {
      files.value = await Promise.all(ordered.map(async (file) => ({
        file,
        path: packagePathOf(file),
        sizeBytes: file.size,
        mediaType: file.type || 'application/octet-stream',
        sha256: await sha256File(file),
        status: 'pending' as const,
        progress: 0,
      })))
      session.value = null
      state.value = { kind: 'ready' }
    } catch (error) {
      state.value = {
        kind: 'error',
        diagnostic: makeDiagnostic('FILE_HASH_FAILED', `计算文件哈希失败：${error instanceof Error ? error.message : String(error)}`, false),
      }
    }
  }

  function removeFile(path: string) {
    files.value = files.value.filter((file) => file.path !== path)
    session.value = null
    state.value = files.value.length > 0 ? { kind: 'ready' } : { kind: 'idle' }
  }

  function clear() {
    files.value = []
    session.value = null
    state.value = { kind: 'idle' }
  }

  async function createSession() {
    const id = projectId.value
    const revision = policyRevision.value
    if (!id || revision === undefined) {
      state.value = { kind: 'error', diagnostic: makeDiagnostic('UPLOAD_NOT_READY', '项目或策略版本缺失，无法创建上传会话。', false) }
      return
    }
    if (files.value.length === 0) {
      state.value = { kind: 'error', diagnostic: makeDiagnostic('UPLOAD_EMPTY', '请至少选择一个文件。', false) }
      return
    }

    state.value = { kind: 'creating' }
    const result = await createProjectProblemPackageUpload({
      path: { projectId: id },
      headers: { 'Idempotency-Key': idempotencyKey() },
      body: {
        projectId: id,
        ...(courseId.value ? { courseId: courseId.value } : {}),
        files: files.value.map((file) => ({ path: file.path, sizeBytes: file.sizeBytes, mediaType: file.mediaType })),
        retentionPolicyRevision: revision,
      },
    })
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      state.value = { kind: 'error', diagnostic: makeDiagnostic(problem?.diagnosticCode ?? 'UPLOAD_SESSION_FAILED', problem?.detail ?? '创建上传会话失败', problem?.retryable ?? true) }
      return
    }
    session.value = result.data
    await uploadFiles(result.data)
  }

  async function uploadFiles(uploadSession: ProblemPackageUploadSessionSchema) {
    state.value = { kind: 'uploading' }
    await Promise.all(uploadSession.uploadTargets.map(async (target) => {
      const file = files.value.find((entry) => entry.path === target.path)
      if (!file || file.status === 'done') return
      file.status = 'uploading'
      try {
        await putFileWithProgress(file.file, target.uploadUrl, target.requiredHeaders, (progress) => { file.progress = progress })
        file.status = 'done'
      } catch (error) {
        file.status = 'error'
        file.error = error instanceof Error ? error.message : String(error)
      }
    }))

    const failed = files.value.filter((file) => file.status === 'error')
    if (failed.length > 0) {
      state.value = { kind: 'error', diagnostic: makeDiagnostic('UPLOAD_OBJECT_FAILED', `以下文件上传失败：${failed.map((file) => file.path).join(', ')}`, true) }
      return
    }
    await complete()
  }

  async function retry() {
    const current = session.value
    const matchesCurrentFiles = current !== null
      && current.files.length === files.value.length
      && files.value.every((file) => current.files.some((sessionFile) => sessionFile.path === file.path))
    if (current && matchesCurrentFiles) await uploadFiles(current)
    else {
      session.value = null
      await createSession()
    }
  }

  function putFileWithProgress(file: File, url: string, headers: Record<string, string>, onProgress: (progress: number) => void): Promise<void> {
    return new Promise((resolve, reject) => {
      const xhr = new XMLHttpRequest()
      xhr.open('PUT', url, true)
      Object.entries(headers).forEach(([key, value]) => xhr.setRequestHeader(key, value))
      xhr.upload.addEventListener('progress', (event) => {
        if (event.lengthComputable) onProgress(Math.round((event.loaded / event.total) * 100))
      })
      xhr.addEventListener('load', () => {
        if (xhr.status >= 200 && xhr.status < 300) resolve()
        else reject(new Error(`上传失败：${xhr.status} ${xhr.statusText}`))
      })
      xhr.addEventListener('error', () => reject(new Error('上传网络错误')))
      xhr.addEventListener('abort', () => reject(new Error('上传已取消')))
      xhr.send(file)
    })
  }

  async function complete() {
    const id = projectId.value
    const uploadId = session.value?.id
    const current = session.value
    if (!id || !uploadId || !current) return
    state.value = { kind: 'completing' }
    const result = await completeProjectProblemPackageUpload({
      path: { projectId: id, uploadId },
      headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(current.revision) },
      body: {},
    })
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      state.value = { kind: 'error', diagnostic: makeDiagnostic(problem?.diagnosticCode ?? 'UPLOAD_COMPLETE_FAILED', problem?.detail ?? '完成材料包上传失败', problem?.retryable ?? true) }
      return
    }
    state.value = { kind: 'done', package: result.data }
  }

  return reactive({ files, session, state, addFiles, addDirectoryItems, removeFile, clear, createSession, retry, formatBytes })
}
