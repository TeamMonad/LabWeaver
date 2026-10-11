const MAX_RETRIES = 2
const RETRYABLE_HTTP_STATUSES = new Set([429, 503])
const RETRY_BASE_DELAY_MS = 1000
const MAX_ERROR_BODY_LENGTH = 8 * 1024
const MAX_ERROR_FIELD_LENGTH = 256

class ObjectStoreUploadError extends Error {
  constructor(
    readonly status: number,
    responseText: string | undefined,
  ) {
    const objectStoreError = readObjectStoreError(responseText)
    const detail = objectStoreError
      ? `；对象存储错误${objectStoreError.code ? ` ${objectStoreError.code}` : ''}${objectStoreError.message ? `：${objectStoreError.message}` : ''}`
      : ''
    super(`对象存储上传失败：HTTP ${status}${detail}`)
    this.name = 'ObjectStoreUploadError'
  }
}

function readObjectStoreError(responseText: string | undefined) {
  if (!responseText) return null
  const body = responseText.slice(0, MAX_ERROR_BODY_LENGTH)
  const readField = (name: string) => body.match(new RegExp(`<${name}>([^<]{1,${MAX_ERROR_FIELD_LENGTH}})</${name}>`, 'i'))?.[1]?.trim() || null
  const code = readField('Code')
  const message = readField('Message')
  const safeCode = code && /^[A-Za-z0-9._-]{1,64}$/.test(code) ? code : null
  const hasControlCharacter = (safeMessageValue: string) => [...safeMessageValue]
    .some((character) => character.charCodeAt(0) < 0x20 || character.charCodeAt(0) === 0x7f)
  const safeMessage = message
    && !/https?:\/\//i.test(message)
    && !hasControlCharacter(message)
    ? message
    : null
  if (!safeCode && !safeMessage) return null
  return { code: safeCode, message: safeMessage }
}

function cancelledError() {
  return new Error('上传已取消。')
}

function isRetryableUploadError(error: unknown): error is ObjectStoreUploadError {
  return error instanceof ObjectStoreUploadError && RETRYABLE_HTTP_STATUSES.has(error.status)
}

function waitForRetry(delayMs: number, signal?: AbortSignal) {
  if (signal?.aborted) return Promise.reject(cancelledError())
  return new Promise<void>((resolve, reject) => {
    let settled = false
    const cleanup = () => signal?.removeEventListener('abort', abort)
    const finish = (callback: () => void) => {
      if (settled) return
      settled = true
      cleanup()
      callback()
    }
    const abort = () => {
      clearTimeout(timeout)
      finish(() => reject(cancelledError()))
    }
    const timeout = setTimeout(() => finish(resolve), delayMs)
    signal?.addEventListener('abort', abort, { once: true })
  })
}

function uploadOnce(
  file: Blob,
  url: string,
  headers: Record<string, string>,
  onProgress: (progress: number) => void,
  signal?: AbortSignal,
): Promise<{ etag: string | null }> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    let settled = false
    const cleanup = () => signal?.removeEventListener('abort', abort)
    const finish = (callback: () => void) => {
      if (settled) return
      settled = true
      cleanup()
      callback()
    }
    const abort = () => {
      xhr.abort()
      rejectCancelled()
    }
    const rejectCancelled = () => finish(() => reject(cancelledError()))
    if (signal?.aborted) {
      rejectCancelled()
      return
    }
    xhr.open('PUT', url, true)
    Object.entries(headers).forEach(([key, value]) => xhr.setRequestHeader(key, value))
    xhr.upload.addEventListener('progress', (event) => {
      if (event.lengthComputable) onProgress(Math.round((event.loaded / event.total) * 100))
    })
    xhr.addEventListener('load', () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        const etag = typeof xhr.getResponseHeader === 'function'
          ? xhr.getResponseHeader('ETag')?.trim() || null
          : null
        finish(() => resolve({ etag }))
      }
      else {
        finish(() => reject(new ObjectStoreUploadError(xhr.status, xhr.responseText)))
      }
    })
    xhr.addEventListener('error', () => finish(() => reject(new Error('对象存储网络错误：无法连接上传端点。'))))
    xhr.addEventListener('abort', rejectCancelled)
    signal?.addEventListener('abort', abort, { once: true })
    xhr.send(file)
  })
}

/**
 * Upload one browser blob straight to a presigned object-store URL.
 *
 * The object store authorizes the exact signed request, so the caller-supplied
 * headers are applied verbatim and the file body is sent unmodified. Progress
 * is reported as a whole percentage so callers can render it directly. A
 * transient 429 or 503 retries the same multipart part with bounded full-jitter
 * backoff; permanent responses and transport errors retain their original
 * failure behavior.
 */
export async function putFileWithProgress(
  file: Blob,
  url: string,
  headers: Record<string, string>,
  onProgress: (progress: number) => void,
  signal?: AbortSignal,
): Promise<{ etag: string | null }> {
  for (let retryCount = 0; retryCount <= MAX_RETRIES; retryCount += 1) {
    try {
      return await uploadOnce(file, url, headers, onProgress, signal)
    }
    catch (error) {
      if (!isRetryableUploadError(error) || retryCount >= MAX_RETRIES) throw error
      const delayMs = Math.random() * RETRY_BASE_DELAY_MS * 2 ** retryCount
      await waitForRetry(delayMs, signal)
    }
  }

  throw new Error('对象存储上传重试状态无效。')
}
