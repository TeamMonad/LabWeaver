import { reactive, ref } from 'vue'
import {
  completePlatformImageUpload,
  createPlatformImageUpload,
  disablePlatformImage,
  listPlatformImages,
  registerPlatformImage,
  repinPlatformImage,
} from '@/generated/contracts'
import type {
  PlatformImageEntryViewSchema,
  PlatformImageKind,
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

export type PlatformImageState =
  | { kind: 'idle' }
  | { kind: 'loading' }
  | { kind: 'ready' }
  | { kind: 'uploading'; progress: number }
  | { kind: 'error'; diagnostic: DiagnosticViewModel }

export interface RegisterPlatformImageInput {
  kind: PlatformImageKind
  binding: string
  /** `<registry-host>/<repository>:<tag>` inside the configured platform registry. */
  sourceReference: string
  trustRevision: number
  reason: string
}

export interface UploadPlatformImageInput {
  kind: PlatformImageKind
  binding: string
  /** Target `<registry-host>/<repository>:<tag>` the imported archive is tagged as. */
  targetReference: string
  trustRevision: number
  reason: string
}

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

  function failure(error: unknown, fallbackCode: string, fallbackMessage: string): void {
    const problem = extractProblemDetails(error)
    state.value = {
      kind: 'error',
      diagnostic: makeDiagnostic(
        problem?.diagnosticCode ?? fallbackCode,
        problem?.detail ?? fallbackMessage,
        problem?.retryable ?? true,
      ),
    }
  }

  async function load(): Promise<void> {
    state.value = { kind: 'loading' }
    const result = await listPlatformImages()
    if (result.error) {
      failure(result.error, 'PLATFORM_IMAGES_LOAD_FAILED', '加载平台镜像目录失败。')
      return
    }
    entries.value = result.data.entries
    state.value = { kind: 'ready' }
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
  async function upload(file: File, input: UploadPlatformImageInput): Promise<boolean> {
    state.value = { kind: 'uploading', progress: 0 }
    const session = await createPlatformImageUpload({
      headers: { 'Idempotency-Key': idempotencyKey() },
      body: {
        ...input,
        archiveBytes: file.size,
        archiveMediaType: PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE,
      },
    })
    if (session.error) {
      failure(session.error, 'PLATFORM_IMAGE_UPLOAD_FAILED', '上传并导入 OCI 归档失败。')
      return false
    }
    try {
      await putFileWithProgress(
        file,
        session.data.uploadTarget.uploadUrl,
        session.data.uploadTarget.requiredHeaders,
        (progress) => {
          state.value = { kind: 'uploading', progress }
        },
      )
    } catch {
      state.value = { kind: 'error', diagnostic: makeDiagnostic('PLATFORM_IMAGE_UPLOAD_FAILED', '上传并导入 OCI 归档失败。', true) }
      return false
    }
    const completion = await completePlatformImageUpload({
      path: { uploadId: session.data.uploadId },
      headers: { 'Idempotency-Key': idempotencyKey(), 'If-Match': ifMatch(session.data.revision) },
      body: {},
    })
    return settle(completion, 'PLATFORM_IMAGE_UPLOAD_FAILED', '上传并导入 OCI 归档失败。')
  }

  function clearDiagnostic(): void {
    if (state.value.kind !== 'error') return
    state.value = entries.value.length > 0 ? { kind: 'ready' } : { kind: 'idle' }
  }

  return reactive({ entries, state, load, register, upload, repin, disable, clearDiagnostic })
}
