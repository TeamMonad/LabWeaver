import { afterEach, describe, expect, it, vi } from 'vitest'

describe('BFF browser session', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
    vi.resetModules()
  })

  it('loads the safe actor, role, and course context without browser OIDC configuration', async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({
      actor: {
        actorId: '01900000-0000-7000-8000-000000000001',
        roles: ['teacher'],
        expiresAt: '2099-01-01T00:00:00.000Z',
      },
      authorizationRevision: 2,
      expiresAt: '2099-01-01T00:00:00.000Z',
      scopes: [
        { kind: 'global' },
        { kind: 'course', course_id: '01900000-0000-7000-8000-000000000002' },
      ],
    }), { status: 200, headers: { 'content-type': 'application/json' } }))
    vi.stubGlobal('fetch', fetch)

    const { useAuth } = await import('@/composables/useAuth')
    const auth = useAuth()
    await auth.loadUser()

    expect(fetch).toHaveBeenCalledWith('/api/v1/auth/session', expect.objectContaining({
      credentials: 'include',
    }))
    expect(auth.isAuthenticated.value).toBe(true)
    expect(auth.user.value?.profile).toMatchObject({
      actor_id: '01900000-0000-7000-8000-000000000001',
      roles: ['teacher'],
      course_id: '01900000-0000-7000-8000-000000000002',
    })
  })
})
