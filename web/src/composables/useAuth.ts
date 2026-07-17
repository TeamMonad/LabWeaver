import { ref, computed } from 'vue'
import { UserManager, type User, WebStorageStateStore } from 'oidc-client-ts'
import { OIDC_CONFIG, OIDC_ENABLED } from '@/config'
import { IS_FIXTURE } from '@/config/dataMode'

const userManager = OIDC_ENABLED
  ? new UserManager({
      ...OIDC_CONFIG,
      userStore: new WebStorageStateStore({ store: window.localStorage }),
    })
  : null

const user = ref<User | null>(null)
const isLoading = ref(false)
const error = ref<Error | null>(null)

/** Returns the current non-expired OIDC bearer for the Public API client. */
export async function getOidcAccessToken(): Promise<string | undefined> {
  if (!userManager) return undefined
  const current = user.value ?? (await userManager.getUser())
  if (!current || current.expired || !current.access_token) return undefined
  user.value = current
  return current.access_token
}

export function useAuth() {
  const isAuthenticated = computed(() => user.value?.expired === false)

  async function login() {
    if (!userManager) {
      error.value = new Error('OIDC is not configured')
      return
    }
    if (__IS_FIXTURE__ && IS_FIXTURE) {
      // The fixture OIDC authority cannot complete a real redirect; the home
      // page renders the deterministic fixture sign-in panel instead.
      window.location.assign('/')
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
    if (__IS_FIXTURE__ && IS_FIXTURE) {
      // Fixture identities live purely in localStorage; clear them locally
      // instead of redirecting to the fake OIDC authority.
      const { signOutFixtureDemo } = await import('@/fixture/devAuth')
      signOutFixtureDemo()
      return
    }
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
