import { ref, computed, watch } from 'vue'
import { defineStore } from 'pinia'

export type ThemePreference = 'system' | 'light' | 'dark'

const STORAGE_KEY = 'labweaver-theme-preference'

function resolveTheme(preference: ThemePreference): 'light' | 'dark' {
  if (preference === 'system') {
    return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
  }
  return preference
}

export const useThemeStore = defineStore('theme', () => {
  const preference = ref<ThemePreference>(
    (localStorage.getItem(STORAGE_KEY) as ThemePreference) || 'system'
  )

  const effectiveTheme = computed<'light' | 'dark'>(() => resolveTheme(preference.value))

  function applyTheme() {
    document.documentElement.setAttribute('data-theme', effectiveTheme.value)
  }

  function setTheme(value: ThemePreference) {
    preference.value = value
    localStorage.setItem(STORAGE_KEY, value)
    applyTheme()
  }

  function listenToSystemTheme() {
    const mediaQuery = window.matchMedia('(prefers-color-scheme: dark)')
    mediaQuery.addEventListener('change', applyTheme)
    applyTheme()
    return () => mediaQuery.removeEventListener('change', applyTheme)
  }

  watch(preference, applyTheme)

  return {
    preference,
    effectiveTheme,
    setTheme,
    applyTheme,
    listenToSystemTheme,
  }
})
