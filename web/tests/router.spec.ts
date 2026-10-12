import { describe, it, expect, vi, beforeEach } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'
import router from '@/router'
import { NAVIGATION_DETAIL_ROUTES, NAVIGATION_GROUPS } from '@/utils/navigation'
import type { User } from 'oidc-client-ts'

const loginMock = vi.fn()
const loadUserMock = vi.fn()

let mockUser: User | null = null

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

function makeUser(roles: unknown, expired = false): User {
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

  it('triggers OIDC login for unauthenticated users accessing role routes', async () => {
    await router.push('/teacher')
    expect(loginMock).toHaveBeenCalled()
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

  it('allows supported platform roles to use the project-scoped Work workbench', async () => {
    mockUser = makeUser(['student'])
    await router.push('/researcher/workspaces')
    expect(router.currentRoute.value.path).toBe('/researcher/workspaces')
  })

  it('normalizes the BFF platform_admin claim and opens the usable admin task', async () => {
    mockUser = makeUser(['platform_admin'])
    await router.push('/admin')
    expect(router.currentRoute.value.path).toBe('/admin/resource-approval')
  })

  it('accepts the legacy comma-separated role claim without inventing a researcher role', async () => {
    mockUser = makeUser('teacher,student')
    await router.push('/researcher/environments')
    expect(router.currentRoute.value.path).toBe('/researcher/environments')
  })
})

describe('single navigation and route config', () => {
  it('routes every navigation item with its label as the title and its project scope', () => {
    for (const group of NAVIGATION_GROUPS) {
      for (const item of group.items) {
        const resolved = router.resolve(item.path)
        expect(resolved.name, item.path).not.toBe('not-found')
        expect(resolved.meta.title).toBe(item.label)
        expect(Boolean(resolved.meta.projectScoped)).toBe(Boolean(item.projectScoped))
        expect(resolved.meta.requiredRoles).toEqual([...group.allowedRoles])
      }
    }
  })

  it('routes detail pages declared next to their navigation item', () => {
    for (const detail of NAVIGATION_DETAIL_ROUTES) {
      const resolved = router.resolve(detail.path.replace(/:\w+/g, 'sample-id'))
      expect(resolved.name).not.toBe('not-found')
      expect(resolved.meta.title).toBe(detail.title)
      expect(resolved.meta.projectScoped).toBe(true)
    }
  })

  it('keeps the home page project-scoped so a link names one project', () => {
    expect(router.resolve('/').meta.projectScoped).toBe(true)
  })
})
