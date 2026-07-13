import { describe, it, expect, beforeEach } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'
import { useRoleStore } from '@/stores/role'

describe('useRoleStore', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
  })

  it('has no role by default', () => {
    const store = useRoleStore()
    expect(store.currentRole).toBeNull()
    expect(store.isAuthenticated).toBe(false)
  })

  it('sets role and updates derived state', () => {
    const store = useRoleStore()
    store.setRole('teacher')
    expect(store.currentRole).toBe('teacher')
    expect(store.isAuthenticated).toBe(true)
    expect(store.roleLabel).toBe('教师')
  })

  it('clears role', () => {
    const store = useRoleStore()
    store.setRole('admin')
    store.clearRole()
    expect(store.currentRole).toBeNull()
    expect(store.isAuthenticated).toBe(false)
  })
})
