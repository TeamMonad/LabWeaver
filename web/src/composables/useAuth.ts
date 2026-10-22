import { ref, computed } from 'vue'
import { UserManager, type User, WebStorageStateStore } from 'oidc-client-ts'
import type { AuthSession, LogoutBrowserSessionResponse } from '@/generated/contracts'
import { API_AUTH_MODE, DIRECT_OIDC_ENABLED, OIDC_CONFIG } from '@/config'
import {
  acknowledgeBffSessionRefreshed,
  subscribeBffCsrfSessionInvalidation,
} from '@/api/bffSessionBarrier'

interface BffUser {
  expired: boolean
  profile: {
    actor_id: string
    roles: string[]
    course_id?: string | null
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
/** Bumped on CSRF session invalidation so App can remount views and clear pending forms. */
const sessionEpoch = ref(0)
/**
 * Monotonic generation for CSRF-driven identity refresh. A failed or superseded
 * refresh must not acknowledge the write gate while a newer refresh is pending.
 */
let bffSessionRefreshGeneration = 0
let bffCsrfSessionListenerInstalled = false
type TopLevelNavigate = (url: string) => void

const navigateTopLevel: TopLevelNavigate = (url) => {
  window.location.assign(url)
}
const SESSION_EXPIRED_MESSAGE = '登录已失效，请重新登录'

function sessionExpiredError(diagnostic: string): Error {
  const error = new Error(SESSION_EXPIRED_MESSAGE)
  Object.defineProperty(error, 'cause', { value: diagnostic, enumerable: false })
  return error
}

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

/** Fetch the current BFF session without mutating module state (caller decides apply). */
async function fetchBffSession(): Promise<BffUser | null> {
  const response = await fetch('/api/v1/auth/session', {
    credentials: 'include',
    headers: { Accept: 'application/json, application/problem+json' },
  })
  if (response.status === 401) return null
  if (!response.ok) throw new Error(`BFF session lookup failed with HTTP ${response.status}`)
  return bffUser(await response.json() as AuthSession)
}

async function loadBffSession(): Promise<void> {
  user.value = await fetchBffSession()
}

/**
 * CSRF rejection means the shared cookie may now belong to another actor.
 * Clear the stale UI actor, bump sessionEpoch (pending forms remount away),
 * re-fetch /api/v1/auth/session, then lift the write gate only if this refresh
 * is still the latest and succeeded — never replay writes; never acknowledge on
 * failed refresh or after a newer invalidation has started.
 */
async function handleBffCsrfSessionInvalidation(): Promise<void> {
  const generation = ++bffSessionRefreshGeneration
  user.value = null
  sessionEpoch.value += 1
  try {
    const next = await fetchBffSession()
    if (generation !== bffSessionRefreshGeneration) return
    user.value = next
    acknowledgeBffSessionRefreshed()
  } catch (err) {
    if (generation !== bffSessionRefreshGeneration) return
    user.value = null
    error.value = err instanceof Error ? err : new Error(String(err))
    // Keep the write gate closed: a failed refresh must not reopen writes.
  }
}

function ensureBffCsrfSessionListener(): void {
  if (bffCsrfSessionListenerInstalled || API_AUTH_MODE !== 'bff') return
  bffCsrfSessionListenerInstalled = true
  subscribeBffCsrfSessionInvalidation(() => {
    void handleBffCsrfSessionInvalidation()
  })
}

/** Returns the current non-expired OIDC bearer for bearer-mode deployments. */
export async function getOidcAccessToken(): Promise<string | undefined> {
  if (!userManager) return undefined
  const current = user.value && 'access_token' in user.value ? user.value : await userManager.getUser()
  if (!current || current.expired || !current.access_token) return undefined
  user.value = current
  return current.access_token
}

export function useAuth(navigate: TopLevelNavigate = navigateTopLevel) {
  ensureBffCsrfSessionListener()
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
    if (isLoading.value) return
    isLoading.value = true
    error.value = null
    try {
      if (API_AUTH_MODE === 'bff') {
        const csrfResponse = await fetch('/api/v1/auth/csrf', {
          credentials: 'include',
          headers: { Accept: 'application/json, application/problem+json' },
        })
        if (csrfResponse.status === 401) {
          user.value = null
          throw sessionExpiredError(`BFF CSRF lookup failed with HTTP ${csrfResponse.status}`)
        }
        if (!csrfResponse.ok) throw new Error(`BFF CSRF lookup failed with HTTP ${csrfResponse.status}`)
        const csrf = await csrfResponse.json() as { csrfToken?: unknown }
        if (typeof csrf.csrfToken !== 'string' || !csrf.csrfToken) throw new Error('BFF returned an invalid CSRF token')
        const response = await fetch('/auth/logout', {
          method: 'POST',
          credentials: 'include',
          headers: {
            Accept: 'application/json, application/problem+json',
            'X-CSRF-Token': csrf.csrfToken,
          },
        })
        if (response.status === 401) {
          user.value = null
          throw sessionExpiredError(`BFF logout failed with HTTP ${response.status}`)
        }
        if (!response.ok) throw new Error(`BFF logout failed with HTTP ${response.status}`)
        const logout = await response.json() as LogoutBrowserSessionResponse
        if (typeof logout.logoutUrl !== 'string' || !logout.logoutUrl) {
          throw new Error('BFF returned an invalid provider logout URL')
        }
        user.value = null
        navigate(logout.logoutUrl)
      } else if (userManager) {
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
    sessionEpoch,
    login,
    logout,
    handleCallback,
    loadUser,
  }
}
