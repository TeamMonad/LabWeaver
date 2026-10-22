import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { effectScope, ref } from 'vue'
import { flushPromises } from '@vue/test-utils'
import { useEnvironmentInstance } from '@/composables/useEnvironmentInstance'
import { getEnvironment } from '@/generated/contracts'

vi.mock('@/generated/contracts', () => ({
  getEnvironment: vi.fn(),
  listProjectResourceLeases: vi.fn(),
  listProjectResourceRequests: vi.fn(),
}))

function environment(observedState: string, operationState = 'succeeded') {
  return {
    data: {
      id: 'env-1',
      projectId: 'project-1',
      observedState,
      desiredState: 'running',
      revision: 3,
      operation: { id: 'op-1', kind: 'start', state: operationState },
    },
    error: undefined,
  } as never
}

let visibility: DocumentVisibilityState = 'visible'

function setVisibility(value: DocumentVisibilityState) {
  visibility = value
  document.dispatchEvent(new Event('visibilitychange'))
}

describe('useEnvironmentInstance polling', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.resetAllMocks()
    visibility = 'visible'
    vi.spyOn(document, 'visibilityState', 'get').mockImplementation(() => visibility)
  })

  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
  })

  function mount() {
    const scope = effectScope()
    const env = scope.run(() => useEnvironmentInstance(ref<string | undefined>('env-1')))!
    return { env, scope }
  }

  it('stops reading once the environment is settled with no active operation', async () => {
    vi.mocked(getEnvironment).mockResolvedValue(environment('ready'))
    const { env, scope } = mount()
    await flushPromises()
    expect(env.instance.kind).toBe('success')
    expect(env.polling).toBe(false)

    await vi.advanceTimersByTimeAsync(30_000)
    expect(getEnvironment).toHaveBeenCalledTimes(1)
    scope.stop()
  })

  it('keeps reading a transitioning environment until it settles', async () => {
    vi.mocked(getEnvironment)
      .mockResolvedValueOnce(environment('stopping', 'running'))
      .mockResolvedValueOnce(environment('stopping', 'running'))
      .mockResolvedValue(environment('stopped'))
    const { env, scope } = mount()
    await flushPromises()
    expect(env.polling).toBe(true)

    await vi.advanceTimersByTimeAsync(3000)
    await vi.advanceTimersByTimeAsync(3000)
    expect(getEnvironment).toHaveBeenCalledTimes(3)
    expect(env.polling).toBe(false)

    await vi.advanceTimersByTimeAsync(30_000)
    expect(getEnvironment).toHaveBeenCalledTimes(3)
    scope.stop()
  })

  it('pauses on a hidden tab and reads once when the tab returns', async () => {
    vi.mocked(getEnvironment).mockResolvedValue(environment('provisioning', 'running'))
    const { env, scope } = mount()
    await flushPromises()
    expect(getEnvironment).toHaveBeenCalledTimes(1)

    setVisibility('hidden')
    await vi.advanceTimersByTimeAsync(30_000)
    expect(getEnvironment).toHaveBeenCalledTimes(1)
    expect(env.polling).toBe(true)

    vi.mocked(getEnvironment).mockResolvedValue(environment('ready'))
    setVisibility('visible')
    await flushPromises()
    expect(getEnvironment).toHaveBeenCalledTimes(2)
    expect(env.polling).toBe(false)
    scope.stop()
  })

  it('re-arms reading when a later load sees a new operation', async () => {
    vi.mocked(getEnvironment).mockResolvedValue(environment('ready'))
    const { env, scope } = mount()
    await flushPromises()
    expect(env.polling).toBe(false)

    vi.mocked(getEnvironment).mockResolvedValueOnce(environment('ready', 'accepted')).mockResolvedValue(environment('stopped'))
    await env.load()
    expect(env.polling).toBe(true)
    await vi.advanceTimersByTimeAsync(3000)
    expect(getEnvironment).toHaveBeenCalledTimes(3)
    expect(env.polling).toBe(false)
    scope.stop()
  })

  it('stops the timer when its owner is disposed', async () => {
    vi.mocked(getEnvironment).mockResolvedValue(environment('provisioning', 'running'))
    const { scope } = mount()
    await flushPromises()
    scope.stop()
    await vi.advanceTimersByTimeAsync(30_000)
    expect(getEnvironment).toHaveBeenCalledTimes(1)
  })
})
