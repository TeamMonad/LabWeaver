import { computed, getCurrentInstance, onUnmounted, reactive, ref } from 'vue'
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
import type {
  PlatformImageEntryViewSchema,
  PlatformImageKind,
  PlatformImageUploadState,
  PlatformImageUploadStatus,
  VirtualMachineDiskFormat,
} from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import { idempotencyKey, ifMatch } from '@/utils/format'
import { putFileWithProgress } from '@/utils/upload'

/**
 * Reviewed OCI layout archive media type accepted by the upload authority.
 *
 * Mirrors `PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE` in the HTTP contract; the browser
 * sends this exact value because the gateway rejects any other media type.
 */
export const PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE = 'application/vnd.oci.image.layout.v1+tar'

/**
 * `If-Match` validator for catalog mutations.
 *
 * The pinned digest in the request body is the concurrency fence for repin and
 * disable; the catalog row carries no revision, so the gateway accepts any
 * current representation and the contract still requires the header.
 */
const ANY_REVISION = '*'

/** Object storage accepts one presigned archive upload up to 5 GB (5,000,000,000 bytes). */
export const MAX_PLATFORM_IMAGE_ARCHIVE_BYTES = 5_000_000_000
const UPLOAD_STORAGE_KEY = 'labweaver.platform-image-upload'
const UPLOAD_POLL_INTERVAL_MS = 1000
const UPLOAD_POLL_TIMEOUT_MS = 15 * 60 * 1000
const PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES = 64 * 1024 * 1024
const PLATFORM_IMAGE_UPLOAD_MAX_CONCURRENCY = 3
const UPLOAD_STATES = new Set<PlatformImageUploadState>([
  'pending',
  'queued',
  'freezing',
  'importing',
  'cancelling',
  'imported',
  'failed',
  'cancelled',
])

type UploadPhase = 'uploading' | 'completing'

interface UploadedPart {
  partNumber: number
  etag: string
  sizeBytes: number
}

interface UploadPartTarget {
  partNumber: number
  uploadUrl: string
  requiredHeaders: Record<string, string>
  expiresAt: string
}

interface MultipartUploadTarget {
  partSizeBytes: number
  parts: UploadPartTarget[]
  expiresAt: string
}

interface MultipartUploadStatus extends PlatformImageUploadStatus {
  uploadTarget?: MultipartUploadTarget | null
  uploadedParts: UploadedPart[]
}

interface PersistedUpload {
  uploadId: string
  revision: number
  completeIdempotencyKey: string
  cancelIdempotencyKey: string
  phase: UploadPhase
  state: PlatformImageUploadState
  archiveBytes: number
}

interface ActiveUpload extends PersistedUpload {
  uploadedParts: UploadedPart[]
}

export type PlatformImageState =
  | { kind: 'idle' }
  | { kind: 'loading' }
  | { kind: 'ready' }
  | { kind: 'uploading'; progress: number; uploadId?: string }
  | { kind: 'processing'; uploadId: string; state: PlatformImageUploadState; revision: number }
  | { kind: 'terminal'; uploadId: string; state: 'imported' | 'failed' | 'cancelled'; revision: number; diagnostic?: DiagnosticViewModel }
  | { kind: 'error'; diagnostic: DiagnosticViewModel; uploadId?: string }

export interface RegisterPlatformImageInput {
  kind: PlatformImageKind
  binding: string
  /** `<registry-host>/<repository>:<tag>` inside the configured platform registry. */
  sourceReference: string
  trustRevision: number
  reason: string
}

/** Container archive upload; the body carries no virtual-machine disk descriptor. */
export interface ContainerUploadPlatformImageInput {
  kind: 'container'
  binding: string
  /** Target `<registry-host>/<repository>:<tag>` the imported archive is tagged as. */
  targetReference: string
  trustRevision: number
  reason: string
}

/**
 * Virtual-machine archive upload.
 *
 * The contract only accepts a base-disk descriptor when all three fields are
 * present, so the type makes them mandatory instead of letting a partially
 * filled form reach the gateway.
 */
export interface VirtualMachineUploadPlatformImageInput {
  kind: 'virtual_machine'
  binding: string
  /** Target `<registry-host>/<repository>:<tag>` the imported archive is tagged as. */
  targetReference: string
  trustRevision: number
  reason: string
  /** Declared base-disk encoding of the disk inside the archive. */
  diskFormat: VirtualMachineDiskFormat
  /** Relative path of the disk inside the uploaded archive, e.g. `disk/disk.img`. */
  diskPath: string
  /** Declared base-disk capacity in bytes; the Agent rejects larger disks. */
  capacityBytes: number
}

export type UploadPlatformImageInput =
  | ContainerUploadPlatformImageInput
  | VirtualMachineUploadPlatformImageInput

/**
 * Administrator platform image catalog over the Control gateway.
 *
 * The Agent authority owns every pinned digest, so mutations never patch local
 * state: each successful mutation re-reads the catalog and the release impact
 * hints from the server. A failed mutation leaves the rendered catalog intact.
 */
export function usePlatformImages() {
  const entries = ref<PlatformImageEntryViewSchema[]>([])
  const state = ref<PlatformImageState>({ kind: 'idle' })
  const activeUpload = ref<ActiveUpload | null>(null)
  const uploadActive = computed(() => activeUpload.value !== null)
  const uploadCompletionRetryable = computed(() => activeUpload.value?.phase === 'completing')
  const uploadCancellationPending = computed(() => activeUpload.value?.state === 'cancelling')
  const uploadNeedsFile = computed(() => (
    activeUpload.value?.phase === 'uploading'
    && state.value.kind === 'error'
    && state.value.uploadId === activeUpload.value.uploadId
  ))
  let uploadPoll: Promise<boolean> | null = null
  let uploadTask: Promise<void> | null = null
  let uploadAbortController: AbortController | null = null
  let uploadStatusMonitorStop: (() => void) | null = null
  let uploadCancelRequested = false
  let disposed = false

  function stopUploadStatusMonitor(): void {
    uploadStatusMonitorStop?.()
    uploadStatusMonitorStop = null
  }

  function monitorUploadStatusDuringTransfer(
    uploadId: string,
    onTerminal: (status: PlatformImageUploadStatus) => void,
  ): void {
    stopUploadStatusMonitor()
    let stopped = false
    let timer: number | null = null
    let resolvePendingRead: (() => void) | null = null
    const stop = () => {
      stopped = true
      if (timer !== null) {
        window.clearTimeout(timer)
        timer = null
      }
      resolvePendingRead?.()
      resolvePendingRead = null
    }
    const waitForNextRead = () => new Promise<void>((resolve) => {
      resolvePendingRead = resolve
      timer = window.setTimeout(() => {
        timer = null
        resolvePendingRead = null
        resolve()
      }, UPLOAD_POLL_INTERVAL_MS)
    })
    const run = (async () => {
      while (!stopped && !disposed && !uploadCancelRequested && activeUpload.value?.uploadId === uploadId) {
        await waitForNextRead()
        if (stopped || disposed || uploadCancelRequested || activeUpload.value?.uploadId !== uploadId) return
        const result = await getPlatformImageUpload({ path: { uploadId } })
        if (stopped || disposed || uploadCancelRequested || activeUpload.value?.uploadId !== uploadId) return
        if (!result.error && ['imported', 'failed', 'cancelled'].includes(result.data.state)) {
          onTerminal(result.data)
          return
        }
      }
    })()
    uploadStatusMonitorStop = stop
    void run.catch(() => undefined)
  }

  function failure(
    error: unknown,
    fallbackCode: string,
    fallbackMessage: string,
    uploadId?: string,
    retryableOverride?: boolean,
  ): void {
    if (disposed) return
    const problem = extractProblemDetails(error)
    state.value = {
      kind: 'error',
      diagnostic: makeDiagnostic(
        problem?.diagnosticCode ?? fallbackCode,
        problem?.detail ?? fallbackMessage,
        retryableOverride ?? problem?.retryable ?? true,
      ),
      ...(uploadId ? { uploadId } : {}),
    }
  }

  function readPersistedUpload(): ActiveUpload | null {
    if (typeof window === 'undefined') return null
    try {
      const raw = window.sessionStorage.getItem(UPLOAD_STORAGE_KEY)
      if (!raw) return null
      const parsed = JSON.parse(raw) as Partial<PersistedUpload>
      if (
        typeof parsed.uploadId !== 'string'
        || !Number.isSafeInteger(parsed.revision)
        || typeof parsed.completeIdempotencyKey !== 'string'
        || typeof parsed.cancelIdempotencyKey !== 'string'
        || (parsed.phase !== 'uploading' && parsed.phase !== 'completing')
        || typeof parsed.state !== 'string'
        || !UPLOAD_STATES.has(parsed.state as PlatformImageUploadState)
        || typeof parsed.archiveBytes !== 'number'
        || !Number.isSafeInteger(parsed.archiveBytes)
        || parsed.archiveBytes < 0
      ) return null
      return { ...parsed, uploadedParts: [] } as ActiveUpload
    } catch {
      return null
    }
  }

  function persistUpload(upload: ActiveUpload | null): void {
    if (typeof window === 'undefined') return
    try {
      if (upload) {
        const persisted: PersistedUpload = {
          uploadId: upload.uploadId,
          revision: upload.revision,
          completeIdempotencyKey: upload.completeIdempotencyKey,
          cancelIdempotencyKey: upload.cancelIdempotencyKey,
          phase: upload.phase,
          state: upload.state,
          archiveBytes: upload.archiveBytes,
        }
        window.sessionStorage.setItem(UPLOAD_STORAGE_KEY, JSON.stringify(persisted))
      }
      else window.sessionStorage.removeItem(UPLOAD_STORAGE_KEY)
    } catch {
      // Storage is an optional refresh aid; it must not block the server flow.
    }
  }

  function restoreActiveState(): void {
    const upload = activeUpload.value
    if (upload) {
      state.value = {
        kind: 'processing',
        uploadId: upload.uploadId,
        state: upload.state,
        revision: upload.revision,
      }
    } else if (entries.value.length > 0) {
      state.value = { kind: 'ready' }
    } else {
      state.value = { kind: 'idle' }
    }
  }

  function statusDiagnostic(status: PlatformImageUploadStatus): DiagnosticViewModel | undefined {
    if (status.state === 'imported') return undefined
    const code = status.diagnostic
      ?? (status.state === 'cancelled' ? 'LW_PLATFORM_IMAGE_UPLOAD_CANCELLED' : 'PLATFORM_IMAGE_UPLOAD_FAILED')
    const message = status.state === 'cancelled'
      ? '镜像导入已取消。'
      : code === 'LW_PLATFORM_IMAGE_UPLOAD_EXPIRED'
        ? '上传会话已过期，请重新选择归档文件上传。'
        : '镜像导入失败，请检查归档后重新上传。'
    return makeDiagnostic(code, message, false)
  }

  function cancellationCompleted(): boolean {
    const current = state.value
    return current.kind === 'terminal' && current.state === 'cancelled'
  }

  function multipartStatus(status: PlatformImageUploadStatus): MultipartUploadStatus {
    const value = status as PlatformImageUploadStatus & Partial<MultipartUploadStatus>
    return {
      ...status,
      uploadedParts: Array.isArray(value.uploadedParts) ? value.uploadedParts : [],
      uploadTarget: value.uploadTarget ?? null,
    }
  }

  function setUploadedParts(upload: ActiveUpload, parts: UploadedPart[]): void {
    const byNumber = new Map<number, UploadedPart>()
    for (const part of parts) {
      if (Number.isInteger(part.partNumber) && part.partNumber > 0 && part.etag.trim().length > 0) {
        byNumber.set(part.partNumber, {
          partNumber: part.partNumber,
          etag: part.etag,
          sizeBytes: part.sizeBytes,
        })
      }
    }
    upload.uploadedParts = [...byNumber.values()].sort((left, right) => left.partNumber - right.partNumber)
    persistUpload(upload)
  }

  function applyStatus(status: PlatformImageUploadStatus): PlatformImageState | null {
    const current = activeUpload.value
    if (disposed || !current || current.uploadId !== status.uploadId) return null
    current.revision = status.revision
    current.state = status.state
    const details = multipartStatus(status)
    if (Object.prototype.hasOwnProperty.call(status, 'uploadedParts')) {
      setUploadedParts(current, details.uploadedParts)
    }
    persistUpload(current)
    if (status.state === 'imported' || status.state === 'failed' || status.state === 'cancelled') {
      const diagnostic = statusDiagnostic(status)
      const terminal: PlatformImageState = {
        kind: 'terminal',
        uploadId: status.uploadId,
        state: status.state,
        revision: status.revision,
        ...(diagnostic ? { diagnostic } : {}),
      }
      activeUpload.value = null
      persistUpload(null)
      return terminal
    }
    state.value = {
      kind: 'processing',
      uploadId: status.uploadId,
      state: status.state,
      revision: status.revision,
    }
    return state.value
  }

  async function readUploadStatus(uploadId: string): Promise<PlatformImageUploadStatus | null> {
    const result = await getPlatformImageUpload({ path: { uploadId } })
    if (disposed) return null
    if (result.error) {
      failure(
        result.error,
        'PLATFORM_IMAGE_UPLOAD_STATUS_FAILED',
        '读取镜像上传状态失败。',
        uploadId,
        activeUpload.value?.phase === 'completing' && activeUpload.value.state !== 'cancelling',
      )
      return null
    }
    return result.data
  }

  async function pollUpload(uploadId: string): Promise<boolean> {
    if (uploadPoll) return uploadPoll
    const run = (async () => {
      const deadline = Date.now() + UPLOAD_POLL_TIMEOUT_MS
      while (!disposed && Date.now() < deadline) {
        const status = await readUploadStatus(uploadId)
        if (!status) return false
        if (disposed) return false
        const next = applyStatus(status)
        if (!next) return false
        if (next.kind === 'terminal') {
          if (next.state === 'imported') await load()
          state.value = next
          return next.state === 'imported'
        }
        await new Promise<void>((resolve) => window.setTimeout(resolve, UPLOAD_POLL_INTERVAL_MS))
      }
      if (disposed) return false
      failure(
        undefined,
        'PLATFORM_IMAGE_UPLOAD_STATUS_TIMEOUT',
        '镜像导入仍未完成。请刷新任务状态后继续。',
        uploadId,
        activeUpload.value?.phase === 'completing' && activeUpload.value.state !== 'cancelling',
      )
      return false
    })()
    const pending = run.finally(() => {
      uploadPoll = null
    })
    uploadPoll = pending
    return pending
  }

  function partByteLength(file: Blob, partSizeBytes: number, partNumber: number): number {
    const start = (partNumber - 1) * partSizeBytes
    if (start < 0 || start >= file.size) return 0
    return Math.min(partSizeBytes, file.size - start)
  }

  function uploadProgress(
    file: Blob,
    partSizeBytes: number,
    uploaded: Map<number, UploadedPart>,
    inFlight: Map<number, number>,
  ): number {
    let completeBytes = 0
    for (let partNumber = 1; ; partNumber += 1) {
      const expectedBytes = partByteLength(file, partSizeBytes, partNumber)
      if (expectedBytes === 0) break
      completeBytes += uploaded.get(partNumber)?.sizeBytes ?? 0
      completeBytes += inFlight.get(partNumber) ?? 0
    }
    return Math.min(100, Math.round((completeBytes / file.size) * 100))
  }

  async function uploadRemainingParts(
    upload: ActiveUpload,
    file: File,
    target: MultipartUploadTarget,
  ): Promise<void> {
    if (target.partSizeBytes !== PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES) {
      throw new Error('平台镜像上传分块大小与服务端不一致。')
    }
    if (file.size !== upload.archiveBytes) {
      throw new Error('所选归档大小与原上传会话不一致，请重新选择同一个归档文件。')
    }

    const uploaded = new Map<number, UploadedPart>()
    for (const part of upload.uploadedParts) {
      const expectedBytes = partByteLength(file, target.partSizeBytes, part.partNumber)
      if (expectedBytes === 0 || part.sizeBytes !== expectedBytes) {
        throw new Error('服务端已上传的归档分块与所选文件不一致。')
      }
      uploaded.set(part.partNumber, part)
    }
    const targets = target.parts
      .filter((part) => !uploaded.has(part.partNumber))
      .sort((left, right) => left.partNumber - right.partNumber)
    const expectedPartCount = Math.ceil(file.size / target.partSizeBytes)
    if (expectedPartCount === 0 || uploaded.size + targets.length !== expectedPartCount) {
      throw new Error('上传会话返回的归档分块清单不完整。')
    }
    const targetNumbers = new Set(targets.map((part) => part.partNumber))
    for (const part of uploaded.values()) {
      if (part.partNumber < 1 || part.partNumber > expectedPartCount) {
        throw new Error('服务端返回了无效的归档分块编号。')
      }
    }
    if (new Set([...uploaded.keys(), ...targetNumbers]).size !== expectedPartCount) {
      throw new Error('上传会话返回了重复或缺失的归档分块。')
    }

    const inFlight = new Map<number, number>()
    const controller = uploadAbortController
    if (!controller) throw new Error('上传任务已停止。')
    let nextTarget = 0
    let firstError: unknown = null
    const updateProgress = () => {
      if (disposed || uploadCancelRequested || activeUpload.value?.uploadId !== upload.uploadId) return
      state.value = {
        kind: 'uploading',
        progress: uploadProgress(file, target.partSizeBytes, uploaded, inFlight),
        uploadId: upload.uploadId,
      }
    }
    updateProgress()

    const worker = async () => {
      while (!firstError && !controller.signal.aborted) {
        const part = targets[nextTarget]
        nextTarget += 1
        if (!part) return
        const expectedBytes = partByteLength(file, target.partSizeBytes, part.partNumber)
        if (expectedBytes === 0) {
          firstError = new Error('上传会话返回了超出归档大小的分块。')
          controller.abort()
          return
        }
        inFlight.set(part.partNumber, 0)
        try {
          const result = await putFileWithProgress(
            file.slice((part.partNumber - 1) * target.partSizeBytes, (part.partNumber - 1) * target.partSizeBytes + expectedBytes),
            part.uploadUrl,
            part.requiredHeaders,
            (progress) => {
              inFlight.set(part.partNumber, Math.round(expectedBytes * progress / 100))
              updateProgress()
            },
            controller.signal,
          )
          if (!result.etag || result.etag.trim().length === 0) {
            throw new Error('对象存储未返回归档分块的 ETag。')
          }
          inFlight.delete(part.partNumber)
          uploaded.set(part.partNumber, {
            partNumber: part.partNumber,
            etag: result.etag,
            sizeBytes: expectedBytes,
          })
          setUploadedParts(upload, [...uploaded.values()])
          updateProgress()
        } catch (error) {
          inFlight.delete(part.partNumber)
          if (!firstError) firstError = error
          controller.abort()
          return
        }
      }
    }
    const workerCount = Math.min(PLATFORM_IMAGE_UPLOAD_MAX_CONCURRENCY, targets.length)
    await Promise.all(Array.from({ length: workerCount }, () => worker()))
    if (firstError) throw firstError
    if (controller.signal.aborted || uploadCancelRequested || disposed) return
    updateProgress()
  }

  async function completeCurrentUpload(upload: ActiveUpload): Promise<boolean> {
    if (disposed || uploadCancelRequested) return false
    if (upload.uploadedParts.length === 0) {
      failure(undefined, 'PLATFORM_IMAGE_UPLOAD_PARTS_MISSING', '归档分块尚未全部上传，无法启动镜像导入。', upload.uploadId, false)
      return false
    }
    upload.phase = 'completing'
    persistUpload(upload)
    const completion = await completePlatformImageUpload({
      path: { uploadId: upload.uploadId },
      headers: { 'Idempotency-Key': upload.completeIdempotencyKey, 'If-Match': ifMatch(upload.revision) },
      body: {
        parts: upload.uploadedParts.map((part) => ({ partNumber: part.partNumber, etag: part.etag })),
      },
    } as never)
    if (disposed || uploadCancelRequested) return false
    if (!completion.error && completion.data) {
      const status = completion.data as unknown as PlatformImageUploadStatus
      if (status.uploadId === upload.uploadId) applyStatus(status)
    }
    const problem = completion.error ? extractProblemDetails(completion.error) : null
    if (completion.error && problem?.retryable === false) {
      failure(completion.error, 'PLATFORM_IMAGE_UPLOAD_FAILED', '启动镜像导入失败。', upload.uploadId)
      return false
    }
    return pollUpload(upload.uploadId)
  }

  async function load(): Promise<void> {
    if (disposed) return
    state.value = { kind: 'loading' }
    const result = await listPlatformImages()
    if (disposed) return
    if (result.error) {
      failure(result.error, 'PLATFORM_IMAGES_LOAD_FAILED', '加载平台镜像目录失败。')
      return
    }
    entries.value = result.data.entries
    restoreActiveState()
  }

  /** Applies one mutation result: reloads the server truth or records the diagnostic. */
  async function settle(
    result: { error?: unknown },
    fallbackCode: string,
    fallbackMessage: string,
  ): Promise<boolean> {
    if (result.error) {
      failure(result.error, fallbackCode, fallbackMessage)
      return false
    }
    await load()
    return true
  }

  async function register(input: RegisterPlatformImageInput): Promise<boolean> {
    const result = await registerPlatformImage({
      headers: { 'Idempotency-Key': idempotencyKey() },
      body: { ...input },
    })
    return settle(result, 'PLATFORM_IMAGE_REGISTER_FAILED', '注册平台镜像失败。')
  }

  async function repin(entry: PlatformImageEntryViewSchema, trustRevision: number, reason: string): Promise<boolean> {
    const result = await repinPlatformImage({
      path: { catalogId: entry.catalogId },
      headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ANY_REVISION },
      body: { expectedDigest: entry.resolvedDigest, trustRevision, reason },
    })
    return settle(result, 'PLATFORM_IMAGE_REPIN_FAILED', '重新固定镜像失败。')
  }

  async function disable(entry: PlatformImageEntryViewSchema, reason: string): Promise<boolean> {
    const result = await disablePlatformImage({
      path: { catalogId: entry.catalogId },
      headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ANY_REVISION },
      body: { expectedDigest: entry.resolvedDigest, reason },
    })
    return settle(result, 'PLATFORM_IMAGE_DISABLE_FAILED', '停用镜像失败。')
  }

  async function transferAndComplete(
    upload: ActiveUpload,
    file: File,
    target: MultipartUploadTarget,
  ): Promise<boolean> {
    uploadCancelRequested = false
    uploadAbortController = new AbortController()
    let transferTerminalStatus: PlatformImageUploadStatus | null = null
    const uploadTaskForSession = uploadRemainingParts(upload, file, target)
    uploadTask = uploadTaskForSession
    monitorUploadStatusDuringTransfer(upload.uploadId, (status) => {
      if (disposed || uploadCancelRequested || activeUpload.value?.uploadId !== upload.uploadId) return
      transferTerminalStatus = status
      const next = applyStatus(status)
      if (next?.kind === 'terminal') {
        state.value = next
        uploadAbortController?.abort()
      }
    })
    try {
      await uploadTaskForSession
      if (transferTerminalStatus) return false
    } catch (error) {
      if (transferTerminalStatus) return false
      if (!disposed && !uploadCancelRequested) {
        const detail = error instanceof Error && error.message.trim().length > 0
          ? error.message
          : '归档分块上传失败。请重新选择同一归档后继续。'
        failure(error, 'PLATFORM_IMAGE_UPLOAD_FAILED', detail, upload.uploadId, false)
      }
      return false
    } finally {
      stopUploadStatusMonitor()
      uploadTask = null
      uploadAbortController = null
    }

    if (transferTerminalStatus || uploadCancelRequested || disposed) return false
    state.value = {
      kind: 'processing',
      uploadId: upload.uploadId,
      state: upload.state,
      revision: upload.revision,
    }
    return completeCurrentUpload(upload)
  }

  /**
   * Resumes the current multipart session when a file is selected again.
   * Presigned targets and observed parts always come from the server status;
   * the browser never treats a local progress value as an uploaded part.
   */
  async function resumeUpload(): Promise<boolean> {
    if (disposed || uploadPoll) return false
    const persisted = activeUpload.value ?? readPersistedUpload()
    if (!persisted) return false

    if (!activeUpload.value) activeUpload.value = persisted
    restoreActiveState()
    const status = await readUploadStatus(persisted.uploadId)
    if (!status) return false
    const next = applyStatus(status)
    if (!next) return false
    if (next.kind === 'terminal') {
      if (next.state === 'imported') {
        await load()
      }
      if (disposed) return false
      state.value = next
      return next.state === 'imported'
    }
    const details = multipartStatus(status)
    if (details.uploadedParts.length > 0) setUploadedParts(persisted, details.uploadedParts)
    if (persisted.phase === 'uploading' && status.state === 'pending') {
      failure(
        undefined,
        'PLATFORM_IMAGE_UPLOAD_FILE_REQUIRED',
        '上传会话仍在等待归档。请重新选择同一个归档文件以继续上传已完成的部分。',
        persisted.uploadId,
        false,
      )
      return false
    }
    if (persisted.phase === 'completing' && status.state === 'pending') {
      if (details.uploadedParts.length === 0) {
        failure(undefined, 'PLATFORM_IMAGE_UPLOAD_PARTS_MISSING', '归档分块状态尚未同步，无法启动镜像导入。', persisted.uploadId, false)
        return false
      }
      return completeCurrentUpload(persisted)
    }
    return pollUpload(persisted.uploadId)
  }

  async function upload(file: File, input: UploadPlatformImageInput): Promise<boolean> {
    if (disposed) return false
    if (file.size > MAX_PLATFORM_IMAGE_ARCHIVE_BYTES) {
      failure(undefined, 'PLATFORM_IMAGE_UPLOAD_TOO_LARGE', '所选归档超过 5 GB 限制。')
      return false
    }

    const existing = activeUpload.value
    if (existing) {
      if (existing.phase !== 'uploading' || existing.state === 'cancelling') {
        failure(undefined, 'PLATFORM_IMAGE_UPLOAD_IN_PROGRESS', '已有镜像上传任务正在处理。')
        return false
      }
      if (file.size !== existing.archiveBytes) {
        failure(undefined, 'PLATFORM_IMAGE_UPLOAD_FILE_MISMATCH', '请重新选择与原上传会话大小相同的归档文件。', existing.uploadId, false)
        return false
      }
      const status = await readUploadStatus(existing.uploadId)
      if (!status) return false
      const next = applyStatus(status)
      if (!next || next.kind === 'terminal') return next?.kind === 'terminal' && next.state === 'imported'
      const details = multipartStatus(status)
      if (status.state !== 'pending') {
        return pollUpload(existing.uploadId)
      }
      if (!details.uploadTarget) {
        failure(
          undefined,
          'PLATFORM_IMAGE_UPLOAD_TARGET_MISSING',
          '上传会话未返回继续上传所需的授权信息，请刷新任务状态后重试。',
          existing.uploadId,
          false,
        )
        return false
      }
      setUploadedParts(existing, details.uploadedParts)
      return transferAndComplete(existing, file, details.uploadTarget)
    }

    uploadCancelRequested = false
    state.value = { kind: 'uploading', progress: 0 }
    const descriptor = input.kind === 'virtual_machine'
      ? { diskFormat: input.diskFormat, diskPath: input.diskPath, capacityBytes: input.capacityBytes }
      : {}
    const session = await createPlatformImageUpload({
      headers: { 'Idempotency-Key': idempotencyKey() },
      body: {
        kind: input.kind,
        binding: input.binding,
        targetReference: input.targetReference,
        trustRevision: input.trustRevision,
        reason: input.reason,
        archiveBytes: file.size,
        archiveMediaType: PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE,
        ...descriptor,
      },
    })
    if (session.error) {
      failure(session.error, 'PLATFORM_IMAGE_UPLOAD_FAILED', '创建镜像上传会话失败。')
      return false
    }

    const sessionData = session.data as unknown as {
      uploadId: string
      archiveBytes: number
      revision: number
      uploadTarget: MultipartUploadTarget
      uploadedParts: UploadedPart[]
    }
    if (
      sessionData.archiveBytes !== file.size
      || !sessionData.uploadTarget
      || sessionData.uploadTarget.partSizeBytes !== PLATFORM_IMAGE_UPLOAD_PART_SIZE_BYTES
      || !Array.isArray(sessionData.uploadTarget.parts)
      || !Array.isArray(sessionData.uploadedParts)
    ) {
      failure(undefined, 'PLATFORM_IMAGE_UPLOAD_TARGET_INVALID', '镜像上传会话返回的分块清单无效。')
      return false
    }

    const active: ActiveUpload = {
      uploadId: sessionData.uploadId,
      revision: sessionData.revision,
      completeIdempotencyKey: idempotencyKey(),
      cancelIdempotencyKey: idempotencyKey(),
      phase: 'uploading',
      state: 'pending',
      archiveBytes: sessionData.archiveBytes,
      uploadedParts: sessionData.uploadedParts,
    }
    activeUpload.value = active
    persistUpload(active)
    // The upload session can finish creating after its view has unmounted.
    // Preserve the nonsensitive session identity for the next visit, but do
    // not start an object-store PUT without a live owner for its progress and
    // cancellation controls.
    if (disposed) return false
    return transferAndComplete(active, file, sessionData.uploadTarget)
  }

  async function retryUploadCompletion(): Promise<boolean> {
    const active = activeUpload.value
    if (!active || active.phase !== 'completing') return false
    return completeCurrentUpload(active)
  }

  async function cancelUpload(): Promise<boolean> {
    const active = activeUpload.value
    if (!active) return false
    if (active.state === 'cancelled') return cancellationCompleted()
    if (active.state === 'imported' || active.state === 'failed') return false
    if (active.state === 'cancelling') return false

    uploadCancelRequested = true
    active.state = 'cancelling'
    persistUpload(active)
    uploadAbortController?.abort()
    if (uploadTask) {
      try {
        await uploadTask
      } catch {
        // The aborted object upload is settled before the session cancellation.
      }
    }
    state.value = {
      kind: 'processing',
      uploadId: active.uploadId,
      state: 'cancelling',
      revision: active.revision,
    }
    const expectedRevision = active.revision
    const result = await cancelPlatformImageUpload({
      path: { uploadId: active.uploadId },
      headers: { 'Idempotency-Key': active.cancelIdempotencyKey, 'If-Match': ifMatch(expectedRevision) },
      body: { expectedRevision },
    })
    if (result.error) {
      const status = await readUploadStatus(active.uploadId)
      if (status) {
        const next = applyStatus(status)
        if (next?.kind === 'terminal') {
          if (next.state === 'imported') await load()
          if (disposed) return false
          state.value = next
          return next.state === 'cancelled'
        }
        if (status.state === 'cancelling') {
          await pollUpload(active.uploadId)
          return cancellationCompleted()
        }
        if (status.revision !== expectedRevision) {
          active.cancelIdempotencyKey = idempotencyKey()
          persistUpload(active)
        }
      } else {
        // A failed status read cannot prove the cancellation request was
        // rejected. Keep the local state fenced as cancelling so refresh will
        // re-read the server instead of offering another cancel against an
        // unknown revision.
        active.state = 'cancelling'
        persistUpload(active)
      }
      failure(result.error, 'PLATFORM_IMAGE_UPLOAD_CANCEL_FAILED', '取消状态暂时无法确认。请刷新任务状态后继续。', active.uploadId, false)
      return false
    }
    await pollUpload(active.uploadId)
    return cancellationCompleted()
  }

  function clearDiagnostic(): void {
    if (state.value.kind !== 'error') return
    if (activeUpload.value) {
      restoreActiveState()
      return
    }
    state.value = entries.value.length > 0 ? { kind: 'ready' } : { kind: 'idle' }
  }

  if (getCurrentInstance()) {
    onUnmounted(() => {
      disposed = true
      stopUploadStatusMonitor()
      uploadAbortController?.abort()
    })
  }

  return reactive({
    entries,
    state,
    uploadActive,
    uploadNeedsFile,
    uploadCompletionRetryable,
    uploadCancellationPending,
    load,
    resumeUpload,
    register,
    upload,
    retryUploadCompletion,
    cancelUpload,
    repin,
    disable,
    clearDiagnostic,
  })
}
