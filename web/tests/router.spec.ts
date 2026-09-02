import { describe, it, expect, vi, beforeEach } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'
import router, { type AppRole } from '@/router'
import type { User } from 'oidc-client-ts'

const loginMock = vi.fn()
const loadUserMock = vi.fn()

let mockUser: User | null = null
const isFixtureMode = import.meta.env.VITE_DATA_MODE === 'fixture'

vi.mock('@/config', () => ({
  OIDC_ENABLED: true,
  OIDC_CONFIG: {
    authority: 'https://auth.example.com',
    client_id: 'labweaver-web',
    redirect_uri: 'http://localhost/auth/callback',
    post_logout_redirect_uri: 'http://localhost/',
    response_type: 'code',
    scope: 'openid profile email',
  },
  // Public API base URL must be an origin (never /api/v1) — see client.ts.
  API_BASE_URL: '/',
  API_AUTH_MODE: 'bff',
  APP_TITLE: 'LabWeaver',
}))

vi.mock('@/composables/useAuth', () => ({
  useAuth: () => ({
    user: { value: mockUser },
    isLoading: { value: false },
    error: { value: null },
    isAuthenticated: { value: mockUser !== null && !mockUser.expired },
    login: loginMock,
    logout: vi.fn(),
    handleCallback: vi.fn(),
    loadUser: loadUserMock,
  }),
}))

function makeUser(roles: AppRole[], expired = false): User {
  return {
    expired,
    profile: { roles },
  } as unknown as User
}

describe('route guard', () => {
  beforeEach(async () => {
    setActivePinia(createPinia())
    mockUser = null
    vi.clearAllMocks()
    await router.push('/')
    await router.isReady()
  })

  it('allows unauthenticated access to home', async () => {
    await router.push('/')
    expect(router.currentRoute.value.path).toBe('/')
  })

  it(`${isFixtureMode ? 'redirects fixture unauthenticated users to home' : 'triggers OIDC login for unauthenticated users accessing role routes'}`, async () => {
    await router.push('/teacher')
    if (isFixtureMode) {
      expect(router.currentRoute.value.name).toBe('home')
    } else {
      expect(loginMock).toHaveBeenCalled()
    }
    // roleRoute redirects to the first child, so the remembered return path is /teacher/overview.
    expect(window.sessionStorage.getItem('auth-return-to')).toBe('/teacher/overview')
  })

  it('blocks users without required role', async () => {
    mockUser = makeUser(['student'])
    await router.push('/teacher')
    expect(router.currentRoute.value.name).toBe('auth-error')
    expect(router.currentRoute.value.query.reason).toBe('role_denied')
  })

  it('allows users with matching role', async () => {
    mockUser = makeUser(['teacher'])
    await router.push('/teacher')
    expect(router.currentRoute.value.path).toBe('/teacher/overview')
  })
})
