import { describe, it, expect, beforeEach } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'
import { useCourseStore } from '@/stores/course'

describe('course store', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
  })

  it('starts with no bound course context', () => {
    const store = useCourseStore()
    expect(store.currentContext).toBeNull()
    expect(store.isBound).toBe(false)
    expect(store.isLoading).toBe(false)
    expect(store.error).toBeNull()
  })

  it('remains unbound when loadContext is called before the backend contract is available', async () => {
    const store = useCourseStore()
    await store.loadContext('user-123')
    expect(store.isBound).toBe(false)
    expect(store.error).toBeNull()
    expect(store.isLoading).toBe(false)
  })

  it('clears context on logout', () => {
    const store = useCourseStore()
    store.clearContext()
    expect(store.currentContext).toBeNull()
    expect(store.isBound).toBe(false)
  })
})
