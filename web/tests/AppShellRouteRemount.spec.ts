import { afterEach, describe, expect, it, vi } from 'vitest'
import { defineComponent, h, ref } from 'vue'
import { flushPromises, mount } from '@vue/test-utils'
import { createPinia } from 'pinia'
import { createMemoryHistory, createRouter } from 'vue-router'

/**
 * #207 / #263: App.vue passes `<RouterView :key="sessionEpoch">` into AppShell's
 * default slot. AppShell must render that slot (not its own unkeyed RouterView),
 * otherwise a CSRF-driven actor switch leaves the old actor's route component,
 * draft input and list mounted. These tests mount the real App + AppShell + Vue
 * Router + real useAuth (BFF mode); only fetch and the shell chrome are stubbed.
 */

function sessionResponse(actorId: string) {
  return new Response(JSON.stringify({
    actor: { actorId, roles: ['student'], expiresAt: '2099-01-01T00:00:00.000Z' },
    authorizationRevision: 1,
    expiresAt: '2099-01-01T00:00:00.000Z',
    scopes: [{ kind: 'global' }],
  }), { status: 200, headers: { 'content-type': 'application/json' } })
}

let mounts = 0
const DraftRoute = defineComponent({
  name: 'DraftRoute',
  setup() {
    mounts += 1
    const draft = ref('')
    return () => h('form', { 'data-testid': 'draft-form' }, [
      h('input', {
        'data-testid': 'draft-input',
        value: draft.value,
        onInput: (event: Event) => { draft.value = (event.target as HTMLInputElement).value },
      }),
    ])
  },
})

function makeRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/:pathMatch(.*)*', component: DraftRoute }],
  })
}

const shellStubs = { TopAppBar: true, NavigationDrawer: true }

describe('AppShell renders the App-provided keyed route tree', () => {
  afterEach(async () => {
    vi.unstubAllGlobals()
    vi.unstubAllEnvs()
    vi.resetModules()
    const { resetBffSessionBarrierForTests } = await import('@/api/bffSessionBarrier')
    resetBffSessionBarrierForTests()
    mounts = 0
  })

  it('remounts the routed page and drops the old draft when a CSRF rejection switches the actor', async () => {
    vi.stubEnv('VITE_API_AUTH_MODE', 'bff')
    const actorA = '01900000-0000-7000-8000-00000000000a'
    const actorB = '01900000-0000-7000-8000-00000000000b'
    const fetch = vi.fn()
      .mockResolvedValueOnce(sessionResponse(actorA))
      .mockResolvedValueOnce(sessionResponse(actorB))
    vi.stubGlobal('fetch', fetch)

    const router = makeRouter()
    await router.push('/student/ssh-keys')
    await router.isReady()
    const { default: App } = await import('@/App.vue')
    const { useAuth } = await import('@/composables/useAuth')
    const { notifyBffCsrfSessionInvalidation, isBffWriteBlocked } = await import('@/api/bffSessionBarrier')

    const wrapper = mount(App, {
      global: { plugins: [createPinia(), router], stubs: shellStubs },
      attachTo: document.body,
    })
    await flushPromises()
    const auth = useAuth()
    expect(auth.user.value?.profile).toMatchObject({ actor_id: actorA })
    expect(wrapper.find('main.app-main [data-testid="draft-input"]').exists()).toBe(true)
    expect(mounts).toBe(1)

    await wrapper.get('[data-testid="draft-input"]').setValue('ssh-ed25519 AAAA old-actor-a-draft')
    expect((wrapper.get('[data-testid="draft-input"]').element as HTMLInputElement).value)
      .toBe('ssh-ed25519 AAAA old-actor-a-draft')

    // Same signal the production API client emits on LW_AUTH_CSRF_REJECTED.
    notifyBffCsrfSessionInvalidation()
    await flushPromises()

    expect(auth.user.value?.profile).toMatchObject({ actor_id: actorB })
    expect(isBffWriteBlocked()).toBe(false)
    expect(mounts).toBe(2)
    expect((wrapper.get('main.app-main [data-testid="draft-input"]').element as HTMLInputElement).value).toBe('')
    expect(wrapper.findAll('[data-testid="draft-form"]')).toHaveLength(1)
    wrapper.unmount()
  })

  it('keeps rendering the router when used without a slot (fixture preview)', async () => {
    const router = makeRouter()
    await router.push('/')
    await router.isReady()
    const { default: AppShell } = await import('@/components/layout/AppShell.vue')
    const wrapper = mount(AppShell, { global: { plugins: [createPinia(), router], stubs: shellStubs } })
    await flushPromises()
    await wrapper.get('main.app-main [data-testid="draft-input"]').setValue('still editable')
    expect((wrapper.get('[data-testid="draft-input"]').element as HTMLInputElement).value).toBe('still editable')
    expect(mounts).toBe(1)
  })
})
