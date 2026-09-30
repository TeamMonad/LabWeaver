/**
 * Upload one browser file straight to a presigned object-store URL.
 *
 * The object store authorizes the exact signed request, so the caller-supplied
 * headers are applied verbatim and the file body is sent unmodified. Progress
 * is reported as a whole percentage so callers can render it directly.
 */
export function putFileWithProgress(
  file: File,
  url: string,
  headers: Record<string, string>,
  onProgress: (progress: number) => void,
  signal?: AbortSignal,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    const cleanup = () => signal?.removeEventListener('abort', abort)
    const abort = () => xhr.abort()
    if (signal?.aborted) {
      reject(new Error('上传已取消'))
      return
    }
    xhr.open('PUT', url, true)
    Object.entries(headers).forEach(([key, value]) => xhr.setRequestHeader(key, value))
    xhr.upload.addEventListener('progress', (event) => {
      if (event.lengthComputable) onProgress(Math.round((event.loaded / event.total) * 100))
    })
    xhr.addEventListener('load', () => {
      cleanup()
      if (xhr.status >= 200 && xhr.status < 300) resolve()
      else reject(new Error(`上传失败：${xhr.status} ${xhr.statusText}`))
    })
    xhr.addEventListener('error', () => {
      cleanup()
      reject(new Error('上传网络错误'))
    })
    xhr.addEventListener('abort', () => {
      cleanup()
      reject(new Error('上传已取消'))
    })
    signal?.addEventListener('abort', abort, { once: true })
    xhr.send(file)
  })
}
