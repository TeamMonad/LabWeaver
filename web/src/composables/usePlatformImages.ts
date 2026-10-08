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

interface PersistedUpload {
  uploadId: string
  revision: number
  completeIdempotencyKey: string
  cancelIdempotencyKey: string
  phase: UploadPhase
  state: PlatformImageUploadState
}

type ActiveUpload = PersistedUpload

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
    const stop = () => {
      stopped = true
      if (timer !== null) {
        window.clearTimeout(timer)
        timer = null
      }
    }
    const waitForNextRead = () => new Promise<void>((resolve) => {
      timer = window.setTimeout(() => {
        timer = null
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
      ) return null
      return parsed as ActiveUpload
    } catch {
      return null
    }
  }

  function persistUpload(upload: ActiveUpload | null): void {
    if (typeof window === 'undefined') return
    try {
      if (upload) window.sessionStorage.setItem(UPLOAD_STORAGE_KEY, JSON.stringify(upload))
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

  function applyStatus(status: PlatformImageUploadStatus): PlatformImageState | null {
    const current = activeUpload.value
    if (disposed || !current || current.uploadId !== status.uploadId) return null
    current.revision = status.revision
    current.state = status.state
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

  async function completeCurrentUpload(upload: ActiveUpload): Promise<boolean> {
    if (disposed || uploadCancelRequested) return false
    upload.phase = 'completing'
    persistUpload(upload)
    const completion = await completePlatformImageUpload({
      path: { uploadId: upload.uploadId },
      headers: { 'Idempotency-Key': upload.completeIdempotencyKey, 'If-Match': ifMatch(upload.revision) },
      body: {},
    })
    if (disposed || uploadCancelRequested) return false
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

  /**
   * Stages one OCI archive and completes the import.
   *
   * Every call creates a fresh upload authority, so a retry after an expired
   * presigned URL never reuses the previous target. The selected file stays
   * with the caller until it explicitly clears it.
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
    if (persisted.phase === 'uploading' && status.state === 'pending') {
      failure(
        undefined,
        'PLATFORM_IMAGE_UPLOAD_FILE_REQUIRED',
        '上次上传中断，浏览器未保存文件。请取消这次上传，再重新选择归档并上传。',
        persisted.uploadId,
        false,
      )
      return false
    }
    if (persisted.phase === 'completing' && status.state === 'pending') {
      return completeCurrentUpload(persisted)
    }
    return pollUpload(persisted.uploadId)
  }

  async function upload(file: File, input: UploadPlatformImageInput): Promise<boolean> {
    if (disposed) return false
    if (activeUpload.value) {
      failure(undefined, 'PLATFORM_IMAGE_UPLOAD_IN_PROGRESS', '已有镜像上传任务正在处理。')
      return false
    }
    if (file.size > MAX_PLATFORM_IMAGE_ARCHIVE_BYTES) {
      failure(undefined, 'PLATFORM_IMAGE_UPLOAD_TOO_LARGE', '所选归档超过 5 GB 限制。')
      return false
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

    const active: ActiveUpload = {
      uploadId: session.data.uploadId,
      revision: session.data.revision,
      completeIdempotencyKey: idempotencyKey(),
      cancelIdempotencyKey: idempotencyKey(),
      phase: 'uploading',
      state: 'pending',
    }
    activeUpload.value = active
    persistUpload(active)
    // The upload session can finish creating after its view has unmounted.
    // Preserve the nonsensitive session identity for the next visit, but do
    // not start an object-store PUT without a live owner for its progress and
    // cancellation controls.
    if (disposed) return false

    uploadAbortController = new AbortController()
    const signal = uploadAbortController.signal
    let transferTerminalStatus: PlatformImageUploadStatus | null = null
    const uploadTaskForSession = putFileWithProgress(
      file,
      session.data.uploadTarget.uploadUrl,
      session.data.uploadTarget.requiredHeaders,
      (progress) => {
        state.value = { kind: 'uploading', progress, uploadId: active.uploadId }
      },
      signal,
    )
    uploadTask = uploadTaskForSession
    monitorUploadStatusDuringTransfer(active.uploadId, (status) => {
      if (disposed || uploadCancelRequested || activeUpload.value?.uploadId !== active.uploadId) return
      transferTerminalStatus = status
      const next = applyStatus(status)
      if (next?.kind === 'terminal') {
        state.value = next
        uploadAbortController?.abort()
      }
    })
    try {
      await uploadTaskForSession
    } catch (error) {
      if (transferTerminalStatus) return false
      if (!disposed && !uploadCancelRequested) {
        const detail = error instanceof Error && error.message.trim().length > 0
          ? error.message
          : '归档上传失败。请检查网络后重新选择文件。'
        failure(error, 'PLATFORM_IMAGE_UPLOAD_FAILED', detail, active.uploadId, false)
      }
      return false
    } finally {
      stopUploadStatusMonitor()
      uploadTask = null
      uploadAbortController = null
    }

    if (uploadCancelRequested || disposed) return false
    state.value = {
      kind: 'processing',
      uploadId: active.uploadId,
      state: active.state,
      revision: active.revision,
    }
    return completeCurrentUpload(active)
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
