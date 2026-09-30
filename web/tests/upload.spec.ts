import { afterEach, describe, expect, it, vi } from 'vitest'
import { putFileWithProgress } from '@/utils/upload'

class MockUploadXhr extends EventTarget {
  status = 0
  statusText = ''
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
})
