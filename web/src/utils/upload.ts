/**
 * Upload one browser blob straight to a presigned object-store URL.
 *
 * The object store authorizes the exact signed request, so the caller-supplied
 * headers are applied verbatim and the file body is sent unmodified. Progress
 * is reported as a whole percentage so callers can render it directly.
 */
export function putFileWithProgress(
  file: Blob,
  url: string,
  headers: Record<string, string>,
  onProgress: (progress: number) => void,
  signal?: AbortSignal,
): Promise<{ etag: string | null }> {
  return new Promise((resolve, reject) => {
    const MAX_ERROR_BODY_LENGTH = 8 * 1024
    const MAX_ERROR_FIELD_LENGTH = 256
    const readObjectStoreError = (responseText: string | undefined) => {
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
    const xhr = new XMLHttpRequest()
    const cleanup = () => signal?.removeEventListener('abort', abort)
    const abort = () => xhr.abort()
    if (signal?.aborted) {
      reject(new Error('上传已取消。'))
      return
    }
    xhr.open('PUT', url, true)
    Object.entries(headers).forEach(([key, value]) => xhr.setRequestHeader(key, value))
    xhr.upload.addEventListener('progress', (event) => {
      if (event.lengthComputable) onProgress(Math.round((event.loaded / event.total) * 100))
    })
    xhr.addEventListener('load', () => {
      cleanup()
      if (xhr.status >= 200 && xhr.status < 300) {
        const etag = typeof xhr.getResponseHeader === 'function'
          ? xhr.getResponseHeader('ETag')?.trim() || null
          : null
        resolve({ etag })
      }
      else {
        const objectStoreError = readObjectStoreError(xhr.responseText)
        const detail = objectStoreError
          ? `；对象存储错误${objectStoreError.code ? ` ${objectStoreError.code}` : ''}${objectStoreError.message ? `：${objectStoreError.message}` : ''}`
          : ''
        reject(new Error(`对象存储上传失败：HTTP ${xhr.status}${detail}`))
      }
    })
    xhr.addEventListener('error', () => {
      cleanup()
      reject(new Error('对象存储网络错误：无法连接上传端点。'))
    })
    xhr.addEventListener('abort', () => {
      cleanup()
      reject(new Error('上传已取消。'))
    })
    signal?.addEventListener('abort', abort, { once: true })
    xhr.send(file)
  })
}
