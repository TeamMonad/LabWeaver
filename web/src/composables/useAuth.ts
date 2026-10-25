import { ref, computed, watch } from 'vue'
import { UserManager, type User, WebStorageStateStore } from 'oidc-client-ts'
import type { AuthSession, LogoutBrowserSessionResponse } from '@/generated/contracts'
import { API_AUTH_MODE, DIRECT_OIDC_ENABLED, OIDC_CONFIG } from '@/config'
import { recordSessionActor, registerSessionConfirmation } from '@/api/session'
import { actorIdFromProfile } from '@/utils/navigation'
import { syncTrackedSubmissionOwner } from '@/utils/trackedSubmissions'
import { syncPendingCommandOwner } from '@/utils/pendingCommands'

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

// Browser-local data belongs to one actor: signing in as someone else, signing
// out or losing the session discards what the previous actor left behind. The
// initial unauthenticated state (before the session is read) is not a sign-out.
watch(() => actorIdFromProfile(user.value?.profile), (actorId, previous) => {
  if (actorId || previous) syncTrackedSubmissionOwner(actorId)
  // Unconfirmed write commands survive an identity that is only temporarily
  // unknown (network error, 503): they are dropped for another actor here and
  // on a confirmed sign-out in `confirmSignedOut`.
  if (actorId) syncPendingCommandOwner(actorId)
  if (!actorId) return
  // Another actor now owns this browser session (for example after a sign-in
  // in another tab): retire the previous actor's pages and in-flight requests.
  // An identity that is only temporarily unknown (network error, 503) keeps the epoch.
  recordSessionActor(actorId)
}, { flush: 'sync' })

// An expired session has ended even though its actor is unchanged: end the
// epoch so nothing read under it decides a page, and start a new one when a
// valid session is read again. Unlike a sign-out, the actor's unconfirmed
// commands are kept, so the same actor can resume them after signing in again.
watch(() => user.value?.expired === true, (expired) => {
  recordSessionActor(expired ? null : actorIdFromProfile(user.value?.profile))
}, { flush: 'sync' })

/** The session is known to have ended: the server said so or the user signed out. */
function confirmSignedOut() {
  user.value = null
  syncPendingCommandOwner(null)
  recordSessionActor(null)
}
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

/** The current BFF actor, or null when the server says the session has ended. */
async function readBffSession(): Promise<BffUser | null> {
  const response = await fetch('/api/v1/auth/session', {
    credentials: 'include',
    headers: { Accept: 'application/json, application/problem+json' },
  })
  if (response.status === 401) return null
  if (!response.ok) throw new Error(`BFF session lookup failed with HTTP ${response.status}`)
  return bffUser(await response.json() as AuthSession)
}

let latestSessionLoad: Promise<void> | undefined

/**
 * Read the BFF session; only the most recently started read decides the actor.
 * A read started before another tab switched the cookie must not overwrite a
 * newer answer, so a superseded read settles with the newest one instead.
 */
function loadBffSession(): Promise<void> {
  const load: Promise<void> = readBffSession().then(
    (next) => {
      if (latestSessionLoad !== load) return latestSessionLoad
      if (next) user.value = next
      else confirmSignedOut()
    },
    (error: unknown) => {
      if (latestSessionLoad !== load) return latestSessionLoad
      throw error
    },
  )
  latestSessionLoad = load
  return load
}

// A CSRF or session rejection re-reads the actor before any further request is sent.
if (API_AUTH_MODE === 'bff') registerSessionConfirmation(loadBffSession)

/** Returns the current non-expired OIDC bearer for bearer-mode deployments. */
export async function getOidcAccessToken(): Promise<string | undefined> {
  if (!userManager) return undefined
  const current = user.value && 'access_token' in user.value ? user.value : await userManager.getUser()
  if (!current || current.expired || !current.access_token) return undefined
  user.value = current
  return current.access_token
}

export function useAuth(navigate: TopLevelNavigate = navigateTopLevel) {
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
          confirmSignedOut()
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
          confirmSignedOut()
          throw sessionExpiredError(`BFF logout failed with HTTP ${response.status}`)
        }
        if (!response.ok) throw new Error(`BFF logout failed with HTTP ${response.status}`)
        const logout = await response.json() as LogoutBrowserSessionResponse
        if (typeof logout.logoutUrl !== 'string' || !logout.logoutUrl) {
          throw new Error('BFF returned an invalid provider logout URL')
        }
        confirmSignedOut()
        navigate(logout.logoutUrl)
      } else if (userManager) {
        await userManager.signoutRedirect()
        confirmSignedOut()
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
      else if (userManager) {
        const current = await userManager.getUser()
        if (current) user.value = current
        else confirmSignedOut()
      }
    } catch (err) {
      // The identity is unknown, not ended: fail closed without discarding the actor's pending commands.
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
