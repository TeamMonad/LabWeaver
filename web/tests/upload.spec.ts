import { afterEach, describe, expect, it, vi } from 'vitest'
import { putFileWithProgress } from '@/utils/upload'

class MockUploadXhr extends EventTarget {
  status = 0
  statusText = ''
  responseText = ''
  upload = { addEventListener: vi.fn() }
  open = vi.fn()
  setRequestHeader = vi.fn()
  send = vi.fn()
  abort = vi.fn(() => this.dispatchEvent(new Event('abort')))
}

describe('putFileWithProgress', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('does not start a PUT when its signal is already aborted', async () => {
    const xhr = new MockUploadXhr()
    vi.stubGlobal('XMLHttpRequest', vi.fn(() => xhr))
    const controller = new AbortController()
    controller.abort()

    await expect(
      putFileWithProgress(new File(['data'], 'image.tar'), 'https://upload.example/image', {}, vi.fn(), controller.signal),
    ).rejects.toThrow('上传已取消')

    expect(xhr.open).not.toHaveBeenCalled()
    expect(xhr.send).not.toHaveBeenCalled()
  })

  it('aborts and settles an in-flight PUT when its signal is aborted', async () => {
    const xhr = new MockUploadXhr()
    vi.stubGlobal('XMLHttpRequest', vi.fn(() => xhr))
    const controller = new AbortController()
    const upload = putFileWithProgress(
      new File(['data'], 'image.tar'),
      'https://upload.example/image',
      {},
      vi.fn(),
      controller.signal,
    )

    expect(xhr.send).toHaveBeenCalledOnce()
    controller.abort()

    await expect(upload).rejects.toThrow('上传已取消')
    expect(xhr.abort).toHaveBeenCalledOnce()
  })

  it('preserves the HTTP status and safe object-store error fields without exposing the response body', async () => {
    const xhr = new MockUploadXhr()
    vi.stubGlobal('XMLHttpRequest', vi.fn(() => xhr))
    const upload = putFileWithProgress(
      new File(['data'], 'image.tar'),
      'https://upload.example/image',
      {},
      vi.fn(),
    )
    xhr.status = 503
    xhr.statusText = 'Service Unavailable'
    xhr.responseText = '<Error><Code>SlowDown</Code><Message>Please retry later</Message><Resource>https://signed.example/object?signature=secret</Resource></Error>'
    xhr.dispatchEvent(new Event('load'))

    await expect(upload).rejects.toThrow('对象存储上传失败：HTTP 503；对象存储错误 SlowDown：Please retry later')
    await expect(upload).rejects.not.toThrow('signed.example')
  })

  it('distinguishes a transport error from an HTTP response', async () => {
    const xhr = new MockUploadXhr()
    vi.stubGlobal('XMLHttpRequest', vi.fn(() => xhr))
    const upload = putFileWithProgress(
      new File(['data'], 'image.tar'),
      'https://upload.example/image',
      {},
      vi.fn(),
    )
    xhr.dispatchEvent(new Event('error'))

    await expect(upload).rejects.toThrow('对象存储网络错误：无法连接上传端点。')
  })
})
