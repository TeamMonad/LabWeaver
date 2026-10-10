import { afterEach, describe, expect, it, vi } from 'vitest'
import { putFileWithProgress } from '@/utils/upload'

class MockUploadXhr extends EventTarget {
  status = 0
  statusText = ''
  responseText = ''
  upload = { addEventListener: vi.fn() }
  open = vi.fn()
  setRequestHeader = vi.fn()
  getResponseHeader = vi.fn()
  send = vi.fn()
  abort = vi.fn(() => this.dispatchEvent(new Event('abort')))
}

describe('putFileWithProgress', () => {
  afterEach(() => {
    vi.restoreAllMocks()
    vi.useRealTimers()
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

  it('does not retry a permanent HTTP response and keeps safe object-store error fields', async () => {
    const xhr = new MockUploadXhr()
    vi.stubGlobal('XMLHttpRequest', vi.fn(() => xhr))
    const upload = putFileWithProgress(
      new File(['data'], 'image.tar'),
      'https://upload.example/image',
      {},
      vi.fn(),
    )
    xhr.status = 403
    xhr.statusText = 'Service Unavailable'
    xhr.responseText = '<Error><Code>SlowDown</Code><Message>Please retry later</Message><Resource>https://signed.example/object?signature=secret</Resource></Error>'
    xhr.dispatchEvent(new Event('load'))

    await expect(upload).rejects.toThrow('对象存储上传失败：HTTP 403；对象存储错误 SlowDown：Please retry later')
    await expect(upload).rejects.not.toThrow('signed.example')
    expect(xhr.open).toHaveBeenCalledOnce()
    expect(xhr.send).toHaveBeenCalledOnce()
  })

  it('retries a 503 with the same signed request and returns the successful ETag', async () => {
    vi.useFakeTimers()
    vi.spyOn(Math, 'random').mockReturnValue(0.5)
    const firstXhr = new MockUploadXhr()
    const secondXhr = new MockUploadXhr()
    const xhrFactory = vi.fn()
      .mockImplementationOnce(() => firstXhr)
      .mockImplementationOnce(() => secondXhr)
    vi.stubGlobal('XMLHttpRequest', xhrFactory)
    vi.mocked(secondXhr.getResponseHeader).mockReturnValue('"etag-retry"')
    const file = new Blob(['same body'])
    const headers = { 'Content-Type': 'application/octet-stream', 'x-amz-meta-test': 'same header' }
    const upload = putFileWithProgress(file, 'https://upload.example/part-1', headers, vi.fn())

    firstXhr.status = 503
    firstXhr.responseText = '<Error><Code>RequestTimeout</Code><Message>retry later</Message></Error>'
    firstXhr.dispatchEvent(new Event('load'))
    await Promise.resolve()
    await vi.advanceTimersByTimeAsync(499)
    expect(xhrFactory).toHaveBeenCalledOnce()
    await vi.advanceTimersByTimeAsync(1)
    expect(xhrFactory).toHaveBeenCalledTimes(2)

    secondXhr.status = 200
    secondXhr.dispatchEvent(new Event('load'))
    await expect(upload).resolves.toEqual({ etag: '"etag-retry"' })
    expect(firstXhr.open).toHaveBeenCalledWith('PUT', 'https://upload.example/part-1', true)
    expect(secondXhr.open).toHaveBeenCalledWith('PUT', 'https://upload.example/part-1', true)
    expect(firstXhr.setRequestHeader.mock.calls).toEqual(secondXhr.setRequestHeader.mock.calls)
    expect(firstXhr.send).toHaveBeenCalledWith(file)
    expect(secondXhr.send).toHaveBeenCalledWith(file)
  })

  it('retries a throttled 429 and then succeeds', async () => {
    vi.useFakeTimers()
    vi.spyOn(Math, 'random').mockReturnValue(0)
    const firstXhr = new MockUploadXhr()
    const secondXhr = new MockUploadXhr()
    const xhrFactory = vi.fn()
      .mockImplementationOnce(() => firstXhr)
      .mockImplementationOnce(() => secondXhr)
    vi.stubGlobal('XMLHttpRequest', xhrFactory)
    const upload = putFileWithProgress(new Blob(['data']), 'https://upload.example/part-2', {}, vi.fn())

    firstXhr.status = 429
    firstXhr.dispatchEvent(new Event('load'))
    await Promise.resolve()
    await vi.runOnlyPendingTimersAsync()
    expect(xhrFactory).toHaveBeenCalledTimes(2)
    secondXhr.status = 204
    secondXhr.dispatchEvent(new Event('load'))

    await expect(upload).resolves.toEqual({ etag: null })
  })

  it('returns the last transient response after the bounded retries are exhausted', async () => {
    vi.useFakeTimers()
    vi.spyOn(Math, 'random').mockReturnValue(0)
    const xhrs = [new MockUploadXhr(), new MockUploadXhr(), new MockUploadXhr()]
    const xhrFactory = vi.fn()
      .mockImplementationOnce(() => xhrs[0])
      .mockImplementationOnce(() => xhrs[1])
      .mockImplementationOnce(() => xhrs[2])
    vi.stubGlobal('XMLHttpRequest', xhrFactory)
    const upload = putFileWithProgress(new Blob(['data']), 'https://upload.example/part-3', {}, vi.fn())

    for (const [index, xhr] of xhrs.entries()) {
      xhr.status = 503
      xhr.responseText = `<Error><Code>SlowDown</Code><Message>attempt ${index + 1}</Message></Error>`
      xhr.dispatchEvent(new Event('load'))
      await Promise.resolve()
      if (index < xhrs.length - 1) await vi.runOnlyPendingTimersAsync()
    }

    await expect(upload).rejects.toThrow('对象存储上传失败：HTTP 503；对象存储错误 SlowDown：attempt 3')
    expect(xhrFactory).toHaveBeenCalledTimes(3)
  })

  it('cancels the retry backoff without starting another PUT', async () => {
    vi.useFakeTimers()
    vi.spyOn(Math, 'random').mockReturnValue(0.5)
    const xhr = new MockUploadXhr()
    const xhrFactory = vi.fn(() => xhr)
    vi.stubGlobal('XMLHttpRequest', xhrFactory)
    const controller = new AbortController()
    const upload = putFileWithProgress(new Blob(['data']), 'https://upload.example/part-4', {}, vi.fn(), controller.signal)

    xhr.status = 503
    xhr.dispatchEvent(new Event('load'))
    await Promise.resolve()
    controller.abort()

    await expect(upload).rejects.toThrow('上传已取消')
    await vi.advanceTimersByTimeAsync(2000)
    expect(xhrFactory).toHaveBeenCalledOnce()
  })

  it('returns the object-store ETag from a successful upload', async () => {
    const xhr = new MockUploadXhr()
    vi.stubGlobal('XMLHttpRequest', vi.fn(() => xhr))
    vi.mocked(xhr.getResponseHeader).mockReturnValue('"etag-1"')
    const upload = putFileWithProgress(
      new Blob(['data']),
      'https://upload.example/part-1',
      {},
      vi.fn(),
    )
    xhr.status = 200
    xhr.dispatchEvent(new Event('load'))

    await expect(upload).resolves.toEqual({ etag: '"etag-1"' })
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
