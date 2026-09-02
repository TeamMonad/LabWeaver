import { describe, it, expect, beforeEach, vi } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'
import { useThemeStore } from '@/stores/theme'

describe('theme store', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    localStorage.clear()
    Object.defineProperty(window, 'matchMedia', {
      writable: true,
      value: vi.fn().mockImplementation((query: string) => ({
        matches: query === '(prefers-color-scheme: dark)',
        media: query,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
      })),
    })
  })

  it('defaults to system preference', () => {
    const store = useThemeStore()
    expect(store.preference).toBe('system')
    expect(store.effectiveTheme).toBe('dark')
  })

  it('can switch to light', () => {
    const store = useThemeStore()
    store.setTheme('light')
    expect(store.preference).toBe('light')
    expect(store.effectiveTheme).toBe('light')
  })

  it('can switch to dark', () => {
    const store = useThemeStore()
    store.setTheme('dark')
    expect(store.preference).toBe('dark')
    expect(store.effectiveTheme).toBe('dark')
  })

  it('restores saved preference from localStorage', () => {
    localStorage.setItem('labweaver-theme-preference', 'light')
    const store = useThemeStore()
    expect(store.preference).toBe('light')
  })
})
