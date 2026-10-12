import { reactive, ref, watch, type Ref } from 'vue'
import {
  cancelProjectProblemPackageUpload,
  completeProjectProblemPackageUpload,
  createProjectProblemPackageUpload,
  getProjectProblemPackage,
  getProjectProblemPackageUpload,
} from '@/generated/contracts'
import type {
  ProblemPackageRetentionChoice,
  ProblemPackageSchema,
  ProblemPackageUploadSessionSchema,
} from '@/generated/contracts'
import { formatBytes, idempotencyKey, ifMatch } from '@/utils/format'
import { isPreconditionFailed, putFileWithProgress } from '@/utils/upload'
import { extractProblemDetails, makeDiagnostic } from '@/types/async'

export type UploadFile = {
  file: File
  path: string
  sizeBytes: number
  mediaType: string
  status: 'pending' | 'uploading' | 'done' | 'error'
  progress: number
  error?: string
  actions?: never
}

export type PackageUploadState =
  | { kind: 'idle' }
  | { kind: 'loading'; message?: string }
  | { kind: 'ready' }
  | { kind: 'creating' }
  | { kind: 'uploading' }
  | { kind: 'completing' }
  | { kind: 'done'; package: ProblemPackageSchema }
  /** A pending upload was found after a refresh; the same files must be chosen again. */
  | { kind: 'needs-files'; missing: string[] }
  | { kind: 'cancelling' }
  | { kind: 'cancelled' }
  | { kind: 'error'; diagnostic: ReturnType<typeof makeDiagnostic> }

/** Keys and revision of one upload operation, persisted so a refresh can resume it. */
type UploadIntent = {
  projectId: string
  /** Files, course and retention the intent was created for. */
  fingerprint: string
  createKey: string
  uploadId?: string
  completeKey?: string
  completeRevision?: number
  cancelKey?: string
  /** The user asked to cancel; retry and refresh continue the cancel, never the upload. */
  cancelRequested?: boolean
  /** The first create request, frozen so a lost response replays exactly the same body. */
  createBody?: CreateUploadBody
}

type CreateUploadBody = {
  projectId: string
  courseId?: string
  files: { path: string; sizeBytes: number; mediaType: string }[]
  retentionPolicyRevision: number
  retentionChoice: ProblemPackageRetentionChoice
}

/** Same bound as platform image part uploads; the limit is not raised. */
const MATERIAL_UPLOAD_MAX_CONCURRENCY = 3

const UPLOAD_INTENT_STORAGE_PREFIX = 'labweaver:problem-package-upload:'

export type ProblemPackageUploadOptions = {
  /** Persist the intent under this scope so a refresh resumes the same upload. */
  recoveryScope?: string
}

/** Project-scoped package upload using the current session/target contract. */
export function useProjectProblemPackageUpload(
  projectId: Ref<string | null>,
  policyRevision: Ref<number | undefined>,
  courseId: Ref<string | null | undefined>,
  retentionChoice: Ref<ProblemPackageRetentionChoice> = ref('finite'),
  options: ProblemPackageUploadOptions = {},
) {
  const files = ref<UploadFile[]>([])
  const session = ref<ProblemPackageUploadSessionSchema | null>(null)
  const state = ref<PackageUploadState>({ kind: 'idle' })
  let loadGeneration = 0
  let intent: UploadIntent | null = null
  const transfers = new Set<AbortController>()

  async function addDirectoryItems(items: DataTransferItemList | null) {
    if (!items) return
    const generation = ++loadGeneration
    const id = projectId.value
    const entries: FileSystemEntry[] = []
    for (let index = 0; index < items.length; index += 1) {
      const entry = items[index].webkitGetAsEntry()
      if (entry) entries.push(entry)
    }
    const collected: File[] = []
    await Promise.all(entries.map((entry) => collectFiles(entry, '', collected)))
    if (generation !== loadGeneration || projectId.value !== id) return
    addFiles(collected)
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

  function addFiles(selected: File[]) {
    ++loadGeneration
    const directoryPrefix = commonDirectoryPrefix(selected.map(rawPackagePathOf))
    const ordered = [...selected].sort((left, right) => packagePathOf(left, directoryPrefix).localeCompare(packagePathOf(right, directoryPrefix)))
    const preparedFiles = ordered.map((file) => ({
      file,
      path: packagePathOf(file, directoryPrefix),
      sizeBytes: file.size,
      mediaType: file.type || 'application/octet-stream',
      status: 'pending' as const,
      progress: 0,
    }))
    files.value = preparedFiles
    // Choosing the same files again continues a recovered upload; anything
    // else is a new operation with a new intent.
    if (!(intent?.uploadId && intent.fingerprint === fingerprintOf(preparedFiles))) {
      session.value = null
      if (intent && !intent.uploadId) writeIntent(null)
    }
    state.value = preparedFiles.length > 0 ? { kind: 'ready' } : { kind: 'idle' }
  }

  function removeFile(path: string) {
    loadGeneration += 1
    files.value = files.value.filter((file) => file.path !== path)
    session.value = null
    if (intent && !intent.uploadId) writeIntent(null)
    state.value = files.value.length > 0 ? { kind: 'ready' } : { kind: 'idle' }
  }

  function reset() {
    loadGeneration += 1
    abortTransfers()
    files.value = []
    session.value = null
    intent = null
    state.value = { kind: 'idle' }
  }

  /** Forget the current selection and its recorded intent (an explicit user action). */
  function clear() {
    const id = projectId.value
    reset()
    const key = id ? storageKey(id) : null
    if (key) {
      try { window.sessionStorage.removeItem(key) } catch { /* storage unavailable */ }
    }
  }

  watch(projectId, (id, previousId) => {
    if (id === previousId) return
    // Another project's recorded intent stays stored so returning resumes it.
    reset()
    if (options.recoveryScope) void resume()
  }, { immediate: Boolean(options.recoveryScope) })

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
          problem,
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

  // ---- Stable intent -------------------------------------------------------
  //
  // One user operation keeps one create key, one completion key + first
  // If-Match, and one cancel key, so a lost response is replayed instead of
  // minting a second intent. The intent is persisted per project and scope so
  // a refresh can query the original upload instead of starting over.

  function fingerprintOf(list: UploadFile[]): string {
    const filesPart = list.map((file) => `${file.path}\u0000${file.sizeBytes}\u0000${file.mediaType}`).join('\u0001')
    return `${courseId.value ?? ''}|${retentionChoice.value}|${filesPart}`
  }

  /** Whether the selected files are the ones the recorded intent was created for. */
  function sameFiles(recorded: UploadIntent, list: UploadFile[]): boolean {
    const frozen = recorded.createBody?.files
    if (!frozen) return recorded.fingerprint === fingerprintOf(list)
    const key = (file: { path: string; sizeBytes: number; mediaType: string }) => `${file.path}\u0000${file.sizeBytes}\u0000${file.mediaType}`
    return frozen.map(key).join('\u0001') === list.map(key).join('\u0001')
  }

  function storageKey(id: string): string | null {
    return options.recoveryScope ? `${UPLOAD_INTENT_STORAGE_PREFIX}${options.recoveryScope}:${id}` : null
  }

  function readIntent(id: string): UploadIntent | null {
    const key = storageKey(id)
    if (!key) return null
    try {
      const raw = window.sessionStorage.getItem(key)
      if (!raw) return null
      const parsed = JSON.parse(raw) as Partial<UploadIntent>
      if (parsed.projectId !== id || typeof parsed.createKey !== 'string' || typeof parsed.fingerprint !== 'string') return null
      return parsed as UploadIntent
    } catch {
      return null
    }
  }

  function writeIntent(next: UploadIntent | null) {
    intent = next
    const id = next?.projectId ?? projectId.value
    if (!id) return
    const key = storageKey(id)
    if (!key) return
    try {
      if (next) window.sessionStorage.setItem(key, JSON.stringify(next))
      else window.sessionStorage.removeItem(key)
    } catch {
      // Storage may be unavailable (private mode, quota). The in-memory intent
      // still protects retries on this page; only refresh recovery is lost.
    }
  }

  function updateIntent(patch: Partial<UploadIntent>) {
    if (intent) writeIntent({ ...intent, ...patch })
  }

  function abortTransfers() {
    for (const controller of transfers) controller.abort()
    transfers.clear()
  }

  function errorState(code: string, message: string, retryable: boolean, problem?: Parameters<typeof makeDiagnostic>[3]): PackageUploadState {
    return { kind: 'error', diagnostic: makeDiagnostic(code, message, retryable, problem) }
  }

  function problemState(error: unknown, fallbackCode: string, fallbackMessage: string): PackageUploadState {
    const problem = extractProblemDetails(error)
    return errorState(problem?.diagnosticCode ?? fallbackCode, problem?.detail ?? fallbackMessage, problem?.retryable ?? true, problem)
  }

  /** Terminal sessions are never revived; the next start creates a new intent. */
  function terminalState(upload: ProblemPackageUploadSessionSchema): PackageUploadState | null {
    switch (upload.state) {
      case 'expired':
        return errorState(upload.terminalDiagnostic ?? 'LW_PACKAGE_UPLOAD_EXPIRED', '上传会话已过期。请重新上传材料包。', false)
      case 'cancelled':
        return { kind: 'cancelled' }
      case 'failed':
        return errorState(upload.terminalDiagnostic ?? 'LW_PACKAGE_UPLOAD_FAILED', '材料包校验失败，没有发布任何文件。请检查文件后重新上传。', false)
      default:
        return null
    }
  }

  function filesMatchSession(upload: ProblemPackageUploadSessionSchema): boolean {
    return upload.files.length === files.value.length
      && files.value.every((file) => upload.files.some((sessionFile) => sessionFile.path === file.path && sessionFile.sizeBytes === file.sizeBytes))
  }

  function isCurrent(generation: number, id: string) {
    return generation === loadGeneration && projectId.value === id
  }

  async function createSession() {
    const id = projectId.value
    const revision = policyRevision.value
    if (!id || revision === undefined) {
      state.value = errorState('UPLOAD_NOT_READY', '项目或策略版本缺失，无法创建上传会话。', false)
      return
    }
    if (files.value.length === 0) {
      state.value = errorState('UPLOAD_EMPTY', '请至少选择一个文件。', false)
      return
    }

    const generation = ++loadGeneration
    const fingerprint = fingerprintOf(files.value)
    // An unfinished upload for exactly these files is continued, not recreated.
    if (intent?.uploadId && intent.projectId === id && intent.fingerprint === fingerprint) {
      await reconcile(intent.uploadId, generation, id)
      return
    }
    if (!intent || intent.projectId !== id || intent.fingerprint !== fingerprint || intent.uploadId || !intent.createBody) {
      writeIntent({
        projectId: id,
        fingerprint,
        createKey: idempotencyKey(),
        createBody: {
          projectId: id,
          ...(courseId.value ? { courseId: courseId.value } : {}),
          files: files.value.map((file) => ({ path: file.path, sizeBytes: file.sizeBytes, mediaType: file.mediaType })),
          retentionPolicyRevision: revision,
          retentionChoice: retentionChoice.value,
        },
      })
    }
    await sendCreate(generation, id)
  }

  /**
   * Send the recorded create request. The body is the one frozen with the key,
   * so a replay after a lost response stays identical even if the policy
   * revision, course or retention choice changed in the meantime.
   */
  async function sendCreate(generation: number, id: string) {
    const recorded = intent
    if (!recorded?.createBody) return
    state.value = { kind: 'creating' }
    const result = await createProjectProblemPackageUpload({
      path: { projectId: id },
      headers: { 'Idempotency-Key': recorded.createKey },
      body: recorded.createBody,
    })
    if (!isCurrent(generation, id)) return
    if (result.error) {
      // The create key stays: a retry replays the same request, and a 409 is
      // reported rather than answered with a second intent.
      state.value = problemState(result.error, 'UPLOAD_SESSION_FAILED', '创建上传会话失败。重试会重发同一个创建请求。')
      return
    }
    session.value = result.data
    updateIntent({ uploadId: result.data.id })
    await continueSession(result.data, generation, id)
  }

  /** Read the original upload, then continue from what the server confirms. */
  async function reconcile(uploadId: string, generation: number, id: string): Promise<void> {
    state.value = { kind: 'loading', message: '正在查询上传进度…' }
    const result = await getProjectProblemPackageUpload({ path: { projectId: id, uploadId } })
    if (!isCurrent(generation, id)) return
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      if (problem?.status === 404) {
        writeIntent(null)
        session.value = null
        state.value = errorState(problem.diagnosticCode ?? 'UPLOAD_SESSION_NOT_FOUND', '找不到原上传会话。请重新上传材料包。', false, problem)
        return
      }
      state.value = problemState(result.error, 'UPLOAD_SESSION_QUERY_FAILED', '暂时无法查询上传进度。重试会继续查询同一个上传会话。')
      return
    }
    session.value = result.data
    await continueSession(result.data, generation, id)
  }

  async function continueSession(upload: ProblemPackageUploadSessionSchema, generation: number, id: string) {
    if (!isCurrent(generation, id)) return
    if (upload.state === 'completed' && upload.completedPackage) {
      finish(upload.completedPackage)
      return
    }
    const terminal = terminalState(upload)
    if (terminal) {
      writeIntent(null)
      session.value = null
      state.value = terminal
      return
    }
    if (intent?.cancelRequested) {
      // The user's cancel stands: never resume PUTs or completion.
      await sendCancel(upload, generation, id)
      return
    }
    if (upload.state === 'completing') {
      // Recover the original completion request with the server's key and revision.
      if (upload.completionIdempotencyKey && upload.completionExpectedRevision != null) {
        updateIntent({ completeKey: upload.completionIdempotencyKey, completeRevision: upload.completionExpectedRevision })
      }
      await complete(generation, id, upload)
      return
    }
    // pending: only the files the server has not confirmed are sent.
    const uploaded = new Set(upload.uploadedPaths)
    for (const file of files.value) {
      if (uploaded.has(file.path)) {
        file.status = 'done'
        file.progress = 100
      }
    }
    const missing = upload.files.map((file) => file.path).filter((path) => !uploaded.has(path))
    if (missing.length > 0 && !filesMatchSession(upload)) {
      // After a refresh the browser no longer holds the files.
      state.value = { kind: 'needs-files', missing }
      return
    }
    await uploadFiles(upload, generation, id)
  }

  async function uploadFiles(uploadSession: ProblemPackageUploadSessionSchema, generation: number, id: string) {
    if (!isCurrent(generation, id)) return
    state.value = { kind: 'uploading' }
    const uploaded = new Set(uploadSession.uploadedPaths)
    let preconditionFailed = false
    const batch = new AbortController()
    transfers.add(batch)
    const queue = uploadSession.uploadTargets.filter((target) => {
      const file = files.value.find((entry) => entry.path === target.path)
      return Boolean(file) && file!.status !== 'done' && !uploaded.has(target.path)
    })
    const worker = async () => {
      // Queued files are taken one at a time, so a cancel stops them from being sent.
      while (!batch.signal.aborted) {
        const target = queue.shift()
        if (!target) return
        const file = files.value.find((entry) => entry.path === target.path)
        if (!file) continue
        file.status = 'uploading'
        file.error = undefined
        try {
          await putFileWithProgress(file.file, target.uploadUrl, target.requiredHeaders, (progress) => { file.progress = progress }, batch.signal)
          file.status = 'done'
        } catch (error) {
          if (batch.signal.aborted) {
            file.status = 'pending'
            return
          }
          if (isPreconditionFailed(error)) preconditionFailed = true
          file.status = 'error'
          file.error = error instanceof Error ? error.message : String(error)
        }
      }
    }
    try {
      await Promise.all(Array.from({ length: Math.min(MATERIAL_UPLOAD_MAX_CONCURRENCY, queue.length) }, () => worker()))
    } finally {
      transfers.delete(batch)
    }

    const afterTransfers = state.value as PackageUploadState
    if (!isCurrent(generation, id) || afterTransfers.kind === 'cancelling' || afterTransfers.kind === 'cancelled') return
    if (preconditionFailed) {
      // Objects are never overwritten; a 412 means the server may already hold
      // it. Ask the session instead of guessing.
      for (const file of files.value) if (file.status === 'error') file.status = 'pending'
      await reconcile(uploadSession.id, generation, id)
      return
    }
    const failed = files.value.filter((file) => file.status === 'error')
    if (failed.length > 0) {
      state.value = errorState('UPLOAD_OBJECT_FAILED', `以下文件上传失败：${failed.map((file) => file.path).join(', ')}。重试会先查询上传会话，只补传缺失文件。`, true)
      return
    }
    await complete(generation, id, uploadSession)
  }

  async function retry() {
    const id = projectId.value
    if (!id) return
    if (intent?.projectId === id && intent.cancelRequested) {
      await cancel()
      return
    }
    const generation = ++loadGeneration
    const recorded = intent?.projectId === id ? intent : null
    // A generic retry continues the recorded intent: it replays the frozen create
    // request or queries the original upload. Course, retention or policy changes
    // since then do not mint a new intent; only choosing other files does.
    if (recorded && !recorded.uploadId && recorded.createBody) {
      await sendCreate(generation, id)
      return
    }
    if (recorded?.uploadId && (files.value.length === 0 || sameFiles(recorded, files.value))) {
      await reconcile(recorded.uploadId, generation, id)
      return
    }
    await createSession()
  }

  async function complete(generation: number, id: string, upload: ProblemPackageUploadSessionSchema, allowRequery = true) {
    if (!isCurrent(generation, id)) return
    if (!intent?.completeKey || intent.completeRevision == null) {
      updateIntent({ completeKey: idempotencyKey(), completeRevision: upload.revision })
    }
    const completeKey = intent?.completeKey ?? idempotencyKey()
    const completeRevision = intent?.completeRevision ?? upload.revision
    state.value = { kind: 'completing' }
    const result = await completeProjectProblemPackageUpload({
      path: { projectId: id, uploadId: upload.id },
      headers: { 'Idempotency-Key': completeKey, 'If-Match': ifMatch(completeRevision) },
      body: {},
    })
    if (!isCurrent(generation, id)) return
    if (result.error) {
      const problem = extractProblemDetails(result.error)
      if (allowRequery && (problem === undefined || problem.status === 412)) {
        // Lost response or stale revision: the session decides what happened.
        state.value = { kind: 'loading', message: '正在确认归档结果…' }
        const queried = await getProjectProblemPackageUpload({ path: { projectId: id, uploadId: upload.id } })
        if (!isCurrent(generation, id)) return
        if (!queried.error) {
          session.value = queried.data
          if (queried.data.state === 'pending') {
            // Completion was never accepted: keep the key, follow the new revision once.
            updateIntent({ completeRevision: queried.data.revision })
            await complete(generation, id, queried.data, false)
            return
          }
          await continueSession(queried.data, generation, id)
          return
        }
      }
      state.value = problemState(result.error, 'UPLOAD_COMPLETE_FAILED', '确认归档失败。重试会先查询上传会话，再沿用原完成请求。')
      return
    }
    finish(result.data)
  }

  function finish(pkg: ProblemPackageSchema) {
    writeIntent(null)
    state.value = { kind: 'done', package: pkg }
  }

  /** Cancel the current upload: latest If-Match, stable cancel key, stop local PUTs. */
  async function cancel() {
    const id = projectId.value
    const uploadId = intent?.projectId === id ? intent?.uploadId : session.value?.id
    abortTransfers()
    if (!id || !uploadId) {
      // Nothing was created on the server; just forget the local intent.
      ++loadGeneration
      writeIntent(null)
      session.value = null
      state.value = files.value.length > 0 ? { kind: 'ready' } : { kind: 'idle' }
      return
    }
    const generation = ++loadGeneration
    // Record the cancel before any request so a failure, retry or refresh keeps it.
    updateIntent({ cancelRequested: true, ...(intent?.cancelKey ? {} : { cancelKey: idempotencyKey() }) })
    state.value = { kind: 'cancelling' }
    const latest = await getProjectProblemPackageUpload({ path: { projectId: id, uploadId } })
    if (!isCurrent(generation, id)) return
    if (latest.error) {
      state.value = problemState(latest.error, 'UPLOAD_CANCEL_FAILED', '暂时无法读取上传会话，取消尚未完成。重试会继续取消。')
      return
    }
    session.value = latest.data
    await continueSession(latest.data, generation, id)
  }

  async function sendCancel(upload: ProblemPackageUploadSessionSchema, generation: number, id: string) {
    if (!intent?.cancelKey) updateIntent({ cancelKey: idempotencyKey() })
    const cancelKey = intent?.cancelKey ?? idempotencyKey()
    abortTransfers()
    state.value = { kind: 'cancelling' }
    const result = await cancelProjectProblemPackageUpload({
      path: { projectId: id, uploadId: upload.id },
      headers: { 'Idempotency-Key': cancelKey, 'If-Match': ifMatch(upload.revision) },
    })
    if (!isCurrent(generation, id)) return
    if (result.error) {
      state.value = problemState(result.error, 'UPLOAD_CANCEL_FAILED', '取消上传失败。重试会沿用同一个取消请求，不会继续上传。')
      return
    }
    session.value = null
    if (result.data.state === 'completed' && result.data.completedPackage) {
      finish(result.data.completedPackage)
      return
    }
    writeIntent(null)
    state.value = { kind: 'cancelled' }
  }

  /** Resume an upload this page started before a refresh, if one is recorded. */
  async function resume(): Promise<void> {
    const id = projectId.value
    if (!id) return
    const recorded = readIntent(id)
    if (!recorded) return
    intent = recorded
    const generation = ++loadGeneration
    if (!recorded.uploadId) {
      // The create response was lost before the refresh: replay the frozen request.
      if (recorded.createBody && !recorded.cancelRequested) await sendCreate(generation, id)
      else writeIntent(null)
      return
    }
    await reconcile(recorded.uploadId, generation, id)
  }

  return reactive({ files, session, state, addFiles, addDirectoryItems, removeFile, clear, loadPackage, createSession, retry, cancel, resume, formatBytes })
}
