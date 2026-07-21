import { ref, computed } from 'vue'
import { UserManager, type User, WebStorageStateStore } from 'oidc-client-ts'
import type { AuthSession } from '@/generated/contracts'
import { API_AUTH_MODE, DIRECT_OIDC_ENABLED, OIDC_CONFIG } from '@/config'
import { IS_FIXTURE } from '@/config/dataMode'

interface BffUser {
  expired: boolean
  profile: {
    actor_id: string
    roles: string[]
    course_id?: string
  }
}

type CurrentUser = User | BffUser

const userManager = API_AUTH_MODE === 'bearer' && DIRECT_OIDC_ENABLED
  ? new UserManager({
      ...OIDC_CONFIG,
      userStore: new WebStorageStateStore({ store: window.localStorage }),
    })
  : null

const user = ref<CurrentUser | null>(null)
const isLoading = ref(false)
const error = ref<Error | null>(null)

function bffUser(session: AuthSession): BffUser {
  const expiresAt = Date.parse(session.expiresAt)
  if (!Number.isFinite(expiresAt)) throw new Error('BFF session returned an invalid expiry')
  const course = session.scopes.find((scope) => 'course_id' in scope)
  return {
    expired: expiresAt <= Date.now(),
    profile: {
      actor_id: session.actor.actorId,
      roles: session.actor.roles.map((role) => role === 'platform_admin' ? 'admin' : role),
      ...(course && 'course_id' in course ? { course_id: course.course_id } : {}),
    },
  }
}

async function loadBffSession(): Promise<void> {
  const response = await fetch('/api/v1/auth/session', {
    credentials: 'include',
    headers: { Accept: 'application/json, application/problem+json' },
  })
  if (response.status === 401) {
    user.value = null
    return
  }
  if (!response.ok) throw new Error(`BFF session lookup failed with HTTP ${response.status}`)
  user.value = bffUser(await response.json() as AuthSession)
}

/** Returns the current non-expired OIDC bearer for bearer-mode deployments. */
export async function getOidcAccessToken(): Promise<string | undefined> {
  if (!userManager) return undefined
  const current = user.value && 'access_token' in user.value ? user.value : await userManager.getUser()
  if (!current || current.expired || !current.access_token) return undefined
  user.value = current
  return current.access_token
}

export function useAuth() {
  const isAuthenticated = computed(() => user.value?.expired === false)

  async function login() {
    if (API_AUTH_MODE === 'bff') {
      const remembered = window.sessionStorage.getItem('auth-return-to')
      const returnTo = remembered || `${window.location.pathname}${window.location.search}${window.location.hash}`
      window.location.assign(`/auth/login?return_to=${encodeURIComponent(returnTo)}`)
      return
    }
    if (!userManager) {
      error.value = new Error('OIDC is not configured')
      return
    }
    if (__IS_FIXTURE__ && IS_FIXTURE) {
      window.location.assign('/')
      return
    }
    isLoading.value = true
    error.value = null
    try {
      await userManager.signinRedirect()
    } catch (err) {
      error.value = err instanceof Error ? err : new Error(String(err))
    } finally {
      isLoading.value = false
    }
  }

  async function handleCallback() {
    isLoading.value = true
    error.value = null
    try {
      if (API_AUTH_MODE === 'bff') await loadBffSession()
      else if (userManager) user.value = await userManager.signinRedirectCallback()
    } catch (err) {
      error.value = err instanceof Error ? err : new Error(String(err))
    } finally {
      isLoading.value = false
    }
  }

  async function logout() {
    isLoading.value = true
    error.value = null
    try {
      if (API_AUTH_MODE === 'bff') {
        const csrfResponse = await fetch('/api/v1/auth/csrf', {
          credentials: 'include',
          headers: { Accept: 'application/json, application/problem+json' },
        })
        if (!csrfResponse.ok) throw new Error(`BFF CSRF lookup failed with HTTP ${csrfResponse.status}`)
        const csrf = await csrfResponse.json() as { csrfToken?: unknown }
        if (typeof csrf.csrfToken !== 'string' || !csrf.csrfToken) throw new Error('BFF returned an invalid CSRF token')
        const response = await fetch('/auth/logout', {
          method: 'POST',
          credentials: 'include',
          headers: { 'X-CSRF-Token': csrf.csrfToken },
        })
        if (!response.ok && response.type !== 'opaqueredirect') {
          throw new Error(`BFF logout failed with HTTP ${response.status}`)
        }
        user.value = null
        window.location.assign('/')
      } else if (userManager) {
        if (__IS_FIXTURE__ && IS_FIXTURE) {
          const { signOutFixtureDemo } = await import('@/fixture/devAuth')
          signOutFixtureDemo()
          return
        }
        await userManager.signoutRedirect()
        user.value = null
      }
    } catch (err) {
      error.value = err instanceof Error ? err : new Error(String(err))
    } finally {
      isLoading.value = false
    }
  }

  async function loadUser() {
    isLoading.value = true
    error.value = null
    try {
      if (API_AUTH_MODE === 'bff') await loadBffSession()
      else if (userManager) user.value = await userManager.getUser()
    } catch (err) {
      user.value = null
      error.value = err instanceof Error ? err : new Error(String(err))
    } finally {
      isLoading.value = false
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
