import { describe, it, expect, beforeEach, vi } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'
import { useCourseStore } from '@/stores/course'

const { listProjects } = vi.hoisted(() => ({ listProjects: vi.fn() }))
vi.mock('@/generated/contracts', () => ({ listProjects }))

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

  it('binds only to a project returned by the Control API', async () => {
    listProjects.mockResolvedValueOnce({
      data: [{ id: 'project-1', name: '真实项目', courseId: null, state: 'active', ownerActorId: 'actor-1', revision: 1, createdAt: '2026-01-01T00:00:00Z', updatedAt: '2026-01-01T00:00:00Z' }],
    })
    const store = useCourseStore()
    await store.loadContext('actor-1')
    expect(store.currentContext).toEqual({ projectId: 'project-1', projectName: '真实项目', courseId: null })
    expect(store.isBound).toBe(true)
    expect(store.error).toBeNull()
    expect(store.isLoading).toBe(false)
  })

  it('fails closed when the Control API cannot load project context', async () => {
    listProjects.mockResolvedValueOnce({ error: new Error('unavailable') })
    const store = useCourseStore()
    await store.loadContext('actor-1')
    expect(store.currentContext).toBeNull()
    expect(store.isBound).toBe(false)
    expect(store.error).toEqual(new Error('unavailable'))
  })

  it('clears context on logout', () => {
    const store = useCourseStore()
    store.clearContext()
    expect(store.currentContext).toBeNull()
    expect(store.isBound).toBe(false)
  })
})
