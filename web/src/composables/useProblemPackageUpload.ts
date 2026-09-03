import { reactive, ref, type Ref } from 'vue'
import { createProblemPackageUpload, completeProblemPackageUpload } from '@/generated/contracts'
import type { ProblemPackageSchema, ProblemPackageUploadSessionSchema } from '@/generated/contracts'
import { IS_FIXTURE } from '@/config/dataMode'
import { computeManifestSha256, sha256File } from '@/utils/crypto'
import { formatBytes, idempotencyKey, ifMatch } from '@/utils/format'
import { extractProblemDetails, makeDiagnostic } from '@/types/async'

export type UploadFile = {
  file: File
  path: string
  sizeBytes: number
  mediaType: string
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

export function useProblemPackageUpload(courseId: Ref<string | undefined>, policyRevision: Ref<number | undefined>) {
  const files = ref<UploadFile[]>([])
  const session = ref<ProblemPackageUploadSessionSchema | null>(null)
  const state = ref<PackageUploadState>({ kind: 'idle' })

  async function addDirectoryItems(items: DataTransferItemList | null) {
    if (!items) return
    const entries: FileSystemEntry[] = []
    for (let i = 0; i < items.length; i++) {
      const entry = items[i].webkitGetAsEntry()
      if (entry) entries.push(entry)
    }
    const collected: File[] = []
    await Promise.all(entries.map((entry) => collectFiles(entry, '', collected)))
    await addFiles(collected)
  }

  async function collectFiles(entry: FileSystemEntry, prefix: string, out: File[]): Promise<void> {
    const path = prefix ? `${prefix}/${entry.name}` : entry.name
    if (entry.isDirectory) {
      const dir = entry as FileSystemDirectoryEntry
      const reader = dir.createReader()
      // Chromium returns directory entries in batches; a single readEntries
      // call silently drops everything past the first batch. Keep reading
      // until an empty batch marks the end of the directory.
      const children: FileSystemEntry[] = []
      for (;;) {
        const batch = await new Promise<FileSystemEntry[]>((resolve, reject) => reader.readEntries(resolve, reject))
        if (batch.length === 0) break
        children.push(...batch)
      }
      await Promise.all(children.map((child) => collectFiles(child, path, out)))
    } else if (entry.isFile) {
      const file = await new Promise<File>((resolve, reject) => (entry as FileSystemFileEntry).file(resolve, reject))
      // Preserve the directory-relative path as the package path.
      Object.defineProperty(file, 'webkitRelativePath', { value: path })
      out.push(file)
    }
  }

  function packagePathOf(file: File): string {
    return (file as File & { webkitRelativePath?: string }).webkitRelativePath ?? file.name
  }

  async function addFiles(selected: File[]) {
    // Directory traversal pushes files in async completion order, so the same
    // folder can produce a different list (and therefore a different
    // manifestSha256) on every run. Sort by package path before hashing so the
    // file table, the manifest hash and visual baselines are deterministic.
    const ordered = [...selected].sort((left, right) => packagePathOf(left).localeCompare(packagePathOf(right)))
    state.value = { kind: 'hashing' }
    try {
      const hashed = await Promise.all(
        ordered.map(async (file) => {
          const path = packagePathOf(file)
          const sha256 = await sha256File(file)
          return {
            file,
            path,
            sizeBytes: file.size,
            mediaType: file.type || 'application/octet-stream',
            sha256,
            status: 'pending' as const,
            progress: 0,
          }
        }),
      )
      files.value = hashed
      state.value = { kind: 'ready' }
    } catch (err) {
      state.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          'FILE_HASH_FAILED',
          `计算文件哈希失败：${err instanceof Error ? err.message : String(err)}`,
          false,
        ),
      }
    }
  }

  function removeFile(path: string) {
    files.value = files.value.filter((f) => f.path !== path)
    // The upload session is bound to the file set it was created for; once the
    // set changes, completing it would archive files that were never sent.
    session.value = null
    if (files.value.length === 0) {
      state.value = { kind: 'idle' }
    }
  }

  function clear() {
    files.value = []
    session.value = null
    state.value = { kind: 'idle' }
  }

  async function createSession() {
    const id = courseId.value
    const rev = policyRevision.value
    if (!id || rev === undefined) {
      state.value = {
        kind: 'error',
        diagnostic: makeDiagnostic('UPLOAD_NOT_READY', '课程上下文或策略版本缺失，无法创建上传会话。', false),
      }
      return
    }
    if (files.value.length === 0) {
      state.value = { kind: 'error', diagnostic: makeDiagnostic('UPLOAD_EMPTY', '请至少选择一个文件。', false) }
      return
    }

    state.value = { kind: 'creating' }
    const result = await createProblemPackageUpload({
      path: { courseId: id },
      headers: { 'Idempotency-Key': idempotencyKey() },
      body: {
        files: files.value.map((f) => ({
          path: f.path,
          sizeBytes: f.sizeBytes,
          sha256: f.sha256,
          mediaType: f.mediaType,
        })),
        retentionPolicyRevision: rev,
      },
    })

    if (result.error) {
      const problem = extractProblemDetails(result.error)
      state.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'UPLOAD_SESSION_FAILED',
          problem?.detail ?? '创建上传会话失败',
          problem?.retryable ?? true,
        ),
      }
      return
    }

    session.value = result.data
    await uploadFiles(result.data)
  }

  async function uploadFiles(uploadSession: ProblemPackageUploadSessionSchema) {
    state.value = { kind: 'uploading' }
    const manifestSha256 = await computeManifestSha256(
      uploadSession.files.map((f) => ({ path: f.path, sizeBytes: f.sizeBytes, sha256: f.sha256, mediaType: f.mediaType })),
    )

    // Every outcome is settled locally: a failed object PUT must never reject
    // the whole batch, otherwise the page would strand in `uploading` with an
    // unhandled rejection instead of reaching the aggregated diagnostic.
    await Promise.all(
      uploadSession.uploadTargets.map(async (target) => {
        const file = files.value.find((f) => f.path === target.path)
        if (!file || file.status === 'done') return
        file.status = 'uploading'
        try {
          if (IS_FIXTURE) {
            // Object-storage egress (presigned PUT) is outside the Public API
            // contract and cannot be intercepted by the fixture adapter, which
            // only wraps the SDK axios transport. Fixture mode marks the upload
            // deterministically; paths tagged `__put-fail__` demonstrate the
            // object-failure diagnostic. The live path below stays the real PUT.
            if (target.path.includes('__put-fail__')) {
              throw new Error('对象上传失败（fixture 确定性失败场景）')
            }
            file.progress = 100
            file.status = 'done'
            return
          }
          await putFileWithProgress(file.file, target.uploadUrl, target.requiredHeaders, (progress) => {
            file.progress = progress
          })
          file.status = 'done'
        } catch (err) {
          file.status = 'error'
          file.error = err instanceof Error ? err.message : String(err)
        }
      }),
    )

    const failed = files.value.filter((f) => f.status === 'error')
    if (failed.length > 0) {
      state.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          'UPLOAD_OBJECT_FAILED',
          `以下文件上传失败：${failed.map((f) => f.path).join(', ')}`,
          true,
        ),
      }
      return
    }

    await complete(manifestSha256)
  }

  /**
   * Retry after an upload error: reuse the existing session when the file set
   * is unchanged (only failed objects are resent); otherwise start a fresh
   * session for the current files.
   */
  async function retry() {
    const current = session.value
    const matchesCurrentFiles =
      current !== null &&
      current.files.length === files.value.length &&
      files.value.every((f) => current.files.some((sf) => sf.path === f.path))
    if (current && matchesCurrentFiles) {
      await uploadFiles(current)
    } else {
      session.value = null
      await createSession()
    }
  }

  function putFileWithProgress(
    file: File,
    url: string,
    headers: Record<string, string>,
    onProgress: (progress: number) => void,
  ): Promise<void> {
    return new Promise((resolve, reject) => {
      const xhr = new XMLHttpRequest()
      xhr.open('PUT', url, true)
      Object.entries(headers).forEach(([k, v]) => xhr.setRequestHeader(k, v))
      xhr.upload.addEventListener('progress', (e) => {
        if (e.lengthComputable) onProgress(Math.round((e.loaded / e.total) * 100))
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

  async function complete(manifestSha256: string) {
    const id = courseId.value
    const uploadId = session.value?.id
    if (!id || !uploadId) return

    state.value = { kind: 'completing' }
    const result = await completeProblemPackageUpload({
      path: { courseId: id, uploadId },
      headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(session.value!.revision) },
      body: { manifestSha256 },
    })

    if (result.error) {
      const problem = extractProblemDetails(result.error)
      state.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'UPLOAD_COMPLETE_FAILED',
          problem?.detail ?? '完成材料包上传失败',
          problem?.retryable ?? true,
        ),
      }
      return
    }

    state.value = { kind: 'done', package: result.data }
  }

  return reactive({
    files,
    session,
    state,
    addFiles,
    addDirectoryItems,
    removeFile,
    clear,
    createSession,
    retry,
    formatBytes,
  })
}
