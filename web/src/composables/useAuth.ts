import { ref, computed } from 'vue'
import { UserManager, type User, WebStorageStateStore } from 'oidc-client-ts'
import { OIDC_CONFIG, OIDC_ENABLED } from '@/config'

const userManager = OIDC_ENABLED
  ? new UserManager({
      ...OIDC_CONFIG,
      userStore: new WebStorageStateStore({ store: window.localStorage }),
    })
  : null

const user = ref<User | null>(null)
const isLoading = ref(false)
const error = ref<Error | null>(null)

export function useAuth() {
  const isAuthenticated = computed(() => user.value?.expired === false)

  async function login() {
    if (!userManager) {
      error.value = new Error('OIDC is not configured')
      return
    }
    isLoading.value = true
    try {
      await userManager.signinRedirect()
    } catch (err) {
      error.value = err instanceof Error ? err : new Error(String(err))
    } finally {
      isLoading.value = false
    }
  }

  async function handleCallback() {
    if (!userManager) return
    isLoading.value = true
    try {
      user.value = await userManager.signinRedirectCallback()
    } catch (err) {
      error.value = err instanceof Error ? err : new Error(String(err))
    } finally {
      isLoading.value = false
    }
  }

  async function logout() {
    if (!userManager) return
    isLoading.value = true
    try {
      await userManager.signoutRedirect()
      user.value = null
    } catch (err) {
      error.value = err instanceof Error ? err : new Error(String(err))
    } finally {
      isLoading.value = false
    }
  }

  async function loadUser() {
    if (!userManager) return
    try {
      user.value = await userManager.getUser()
    } catch (err) {
      error.value = err instanceof Error ? err : new Error(String(err))
    }
  }

  return {
    user,
    isLoading,
    error,
    isAuthenticated,
    login,
    logout,
    handleCallback,
    loadUser,
  }
}
