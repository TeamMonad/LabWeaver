import { reactive, ref, watch, type Ref } from 'vue'
import {
  completeProjectProblemPackageUpload,
  createProjectProblemPackageUpload,
  getProjectProblemPackage,
} from '@/generated/contracts'
import type {
  ProblemPackageSchema,
  ProblemPackageUploadSessionSchema,
} from '@/generated/contracts'
import { sha256File } from '@/utils/crypto'
import { formatBytes, idempotencyKey, ifMatch } from '@/utils/format'
import { putFileWithProgress } from '@/utils/upload'
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
  | { kind: 'loading'; message?: string }
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
  let loadGeneration = 0

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

  function rawPackagePathOf(file: File): string {
    const relativePath = (file as File & { webkitRelativePath?: string }).webkitRelativePath
    return relativePath || file.name
  }

  function commonDirectoryPrefix(paths: string[]): string | undefined {
    if (paths.length === 0) return undefined
    const firstSegment = paths[0].split('/')[0]
    if (!firstSegment || !paths.every((path) => path.split('/')[0] === firstSegment)) return undefined
    return firstSegment
  }

  function packagePathOf(file: File, directoryPrefix?: string): string {
    const path = rawPackagePathOf(file)
    if (!directoryPrefix) return path
    const prefix = `${directoryPrefix}/`
    return path.startsWith(prefix) ? path.slice(prefix.length) : path
  }

  async function addFiles(selected: File[]) {
    const generation = ++loadGeneration
    const id = projectId.value
    const directoryPrefix = commonDirectoryPrefix(selected.map(rawPackagePathOf))
    const ordered = [...selected].sort((left, right) => packagePathOf(left, directoryPrefix).localeCompare(packagePathOf(right, directoryPrefix)))
    state.value = { kind: 'hashing' }
    try {
      const preparedFiles = await Promise.all(ordered.map(async (file) => ({
        file,
        path: packagePathOf(file, directoryPrefix),
        sizeBytes: file.size,
        mediaType: file.type || 'application/octet-stream',
        sha256: await sha256File(file),
        status: 'pending' as const,
        progress: 0,
      })))
      if (generation !== loadGeneration || projectId.value !== id) return
      files.value = preparedFiles
      session.value = null
      state.value = { kind: 'ready' }
    } catch (error) {
      if (generation !== loadGeneration || projectId.value !== id) return
      state.value = {
        kind: 'error',
        diagnostic: makeDiagnostic('FILE_HASH_FAILED', `计算文件哈希失败：${error instanceof Error ? error.message : String(error)}`, false),
      }
    }
  }

  function removeFile(path: string) {
    loadGeneration += 1
    files.value = files.value.filter((file) => file.path !== path)
    session.value = null
    state.value = files.value.length > 0 ? { kind: 'ready' } : { kind: 'idle' }
  }

  function clear() {
    loadGeneration += 1
    files.value = []
    session.value = null
    state.value = { kind: 'idle' }
  }

  watch(projectId, (id, previousId) => {
    if (id === previousId) return
    clear()
  })

  /**
   * Restore an already completed package from the project-scoped API.
   *
   * A completed package has no browser File objects to rehydrate. Keeping the
   * server projection as `done` lets the authoring page resume the next
   * operation after a refresh without inventing a second client-side package
   * state machine. The response is checked against the requested project and
   * package id so a stale or cross-project route cannot become active context.
   */
  async function loadPackage(packageId: string): Promise<boolean> {
    const id = projectId.value
    const normalizedPackageId = packageId.trim()
    const generation = ++loadGeneration
    if (!id || !normalizedPackageId) {
      state.value = {
        kind: 'error',
        diagnostic: makeDiagnostic('UPLOAD_PACKAGE_CONTEXT_MISSING', '缺少项目或材料包引用，无法恢复。', false),
      }
      return false
    }

    state.value = { kind: 'loading', message: '读取已归档材料包…' }
    const result = await getProjectProblemPackage({
      path: { projectId: id, packageId: normalizedPackageId },
    })
    if (generation !== loadGeneration || projectId.value !== id) return false
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      state.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'UPLOAD_PACKAGE_LOAD_FAILED',
          problem?.detail ?? '读取已归档材料包失败。请重新加载或从材料上传开始。',
          problem?.retryable ?? true,
        ),
      }
      return false
    }
    if (result.data.id !== normalizedPackageId || result.data.projectId !== id) {
      state.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          'UPLOAD_PACKAGE_STALE_CONTEXT',
          '材料包引用已过期或不属于当前项目，请从当前项目重新选择材料。',
          false,
        ),
      }
      return false
    }

    files.value = []
    session.value = null
    state.value = { kind: 'done', package: result.data }
    return true
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

    const generation = ++loadGeneration
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
    if (generation !== loadGeneration || projectId.value !== id) return
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      state.value = { kind: 'error', diagnostic: makeDiagnostic(problem?.diagnosticCode ?? 'UPLOAD_SESSION_FAILED', problem?.detail ?? '创建上传会话失败', problem?.retryable ?? true) }
      return
    }
    session.value = result.data
    await uploadFiles(result.data, generation, id)
  }

  async function uploadFiles(uploadSession: ProblemPackageUploadSessionSchema, generation: number, id: string) {
    if (generation !== loadGeneration || projectId.value !== id) return
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

    if (generation !== loadGeneration || projectId.value !== id) return
    const failed = files.value.filter((file) => file.status === 'error')
    if (failed.length > 0) {
      state.value = { kind: 'error', diagnostic: makeDiagnostic('UPLOAD_OBJECT_FAILED', `以下文件上传失败：${failed.map((file) => file.path).join(', ')}`, true) }
      return
    }
    await complete(generation, id)
  }

  async function retry() {
    const current = session.value
    const matchesCurrentFiles = current !== null
      && current.files.length === files.value.length
      && files.value.every((file) => current.files.some((sessionFile) => sessionFile.path === file.path))
    if (current && matchesCurrentFiles) {
      const id = projectId.value
      if (!id) return
      const generation = ++loadGeneration
      await uploadFiles(current, generation, id)
    }
    else {
      session.value = null
      await createSession()
    }
  }

  async function complete(generation: number, id: string) {
    const uploadId = session.value?.id
    const current = session.value
    if (generation !== loadGeneration || projectId.value !== id || !uploadId || !current) return
    state.value = { kind: 'completing' }
    const result = await completeProjectProblemPackageUpload({
      path: { projectId: id, uploadId },
      headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(current.revision) },
      body: {},
    })
    if (generation !== loadGeneration || projectId.value !== id) return
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      state.value = { kind: 'error', diagnostic: makeDiagnostic(problem?.diagnosticCode ?? 'UPLOAD_COMPLETE_FAILED', problem?.detail ?? '完成材料包上传失败', problem?.retryable ?? true) }
      return
    }
    state.value = { kind: 'done', package: result.data }
  }

  return reactive({ files, session, state, addFiles, addDirectoryItems, removeFile, clear, loadPackage, createSession, retry, formatBytes })
}
