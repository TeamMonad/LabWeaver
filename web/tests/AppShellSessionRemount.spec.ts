import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { enableAutoUnmount, flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { defineComponent, h, ref } from 'vue'
import { createMemoryHistory, createRouter, type RouteRecordRaw } from 'vue-router'
// Each test re-imports the shell after vi.resetModules() so module-level auth,
// session and project state start fresh. Loading the graph once here moves the
// cold transform into file collection, which has no per-test timeout.
import '@/components/layout/AppShell.vue'

const listProjects = vi.hoisted(() => vi.fn())
const getProject = vi.hoisted(() => vi.fn())

vi.mock('@/generated/contracts', () => ({ listProjects, getProject }))

const projectA = { id: 'project-a', name: '项目 A', state: 'active', revision: 1, ownerActorId: 'u', createdAt: '', updatedAt: '' }
const projectB = { id: 'project-b', name: '项目 B', state: 'active', revision: 1, ownerActorId: 'u', createdAt: '', updatedAt: '' }
const actorA = '01900000-0000-7000-8000-00000000000a'
const actorB = '01900000-0000-7000-8000-00000000000b'

function sessionOf(actorId: string) {
  return new Response(JSON.stringify({
    actor: { actorId, roles: ['student'], expiresAt: '2099-01-01T00:00:00.000Z' },
    authorizationRevision: 1,
    expiresAt: '2099-01-01T00:00:00.000Z',
    scopes: [{ kind: 'global' }],
  }), { status: 200, headers: { 'content-type': 'application/json' } })
}

let mounts = 0
const DraftPage = defineComponent({
  name: 'DraftPage',
  setup() {
    mounts += 1
    const draft = ref('')
    return () => h('input', {
      'data-testid': 'draft',
      value: draft.value,
      onInput: (event: Event) => { draft.value = (event.target as HTMLInputElement).value },
    })
  },
})

const routes: RouteRecordRaw[] = [
  { path: '/student/ssh-keys', component: DraftPage, meta: { title: 'SSH 公钥', projectScoped: true } },
  { path: '/admin/platform', component: DraftPage, meta: { title: '平台配置' } },
]

enableAutoUnmount(afterEach)

/** Mounts the shell signed in as actor A; the next session read answers as actor B. */
async function mountAsActorA(path: string) {
  vi.resetModules()
  vi.stubEnv('VITE_API_AUTH_MODE', 'bff')
  vi.stubGlobal('fetch', vi.fn()
    .mockResolvedValueOnce(sessionOf(actorA))
    .mockResolvedValueOnce(sessionOf(actorB)))
  const { default: AppShell } = await import('@/components/layout/AppShell.vue')
  const { useAuth } = await import('@/composables/useAuth')
  const { useProjects } = await import('@/composables/useProjects')
  const router = createRouter({ history: createMemoryHistory(), routes })
  await router.push(path)
  await router.isReady()
  const auth = useAuth()
  await auth.loadUser()
  const wrapper = mount(AppShell, {
    global: { plugins: [router], stubs: { TopAppBar: true, NavigationDrawer: true } },
  })
  await useProjects().load()
  await flushPromises()
  return { wrapper, auth, router }
}

describe('AppShell after another actor takes over the browser session (#198)', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    localStorage.clear()
    mounts = 0
    listProjects.mockReset()
    getProject.mockReset()
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.unstubAllEnvs()
  })

  it('drops the previous actor\'s page and draft and waits for the new actor\'s project list', async () => {
    listProjects.mockResolvedValue({ data: [projectA], error: undefined })
    const { wrapper, auth } = await mountAsActorA('/student/ssh-keys?projectId=project-a')
    await wrapper.get('[data-testid="draft"]').setValue('ssh-ed25519 AAAA actor-a-draft')
    expect(mounts).toBe(1)

    let answerList!: (value: unknown) => void
    const actorBList = new Promise((resolve) => { answerList = resolve })
    listProjects.mockReturnValue(actorBList)
    await auth.loadUser()
    await flushPromises()

    // Actor B's list is not known yet: the shell renders no page rather than actor A's.
    expect(wrapper.find('[data-testid="draft"]').exists()).toBe(false)
    expect(wrapper.text()).toContain('正在确认项目…')

    answerList({ data: [projectA, projectB], error: undefined })
    await flushPromises()
    expect(mounts).toBe(2)
    expect((wrapper.get('[data-testid="draft"]').element as HTMLInputElement).value).toBe('')
  })

  it('re-creates a page outside any project when the actor changes', async () => {
    listProjects.mockResolvedValue({ data: [projectA], error: undefined })
    const { wrapper, auth } = await mountAsActorA('/admin/platform')
    await wrapper.get('[data-testid="draft"]').setValue('actor A unsaved setting')

    await auth.loadUser()
    await flushPromises()

    expect(mounts).toBe(2)
    expect((wrapper.get('[data-testid="draft"]').element as HTMLInputElement).value).toBe('')
  })

  it('keeps the project context recovery when the new actor cannot open the linked project', async () => {
    listProjects.mockResolvedValue({ data: [projectA], error: undefined })
    const { wrapper, auth } = await mountAsActorA('/student/ssh-keys?projectId=project-a')
    expect(mounts).toBe(1)

    listProjects.mockResolvedValue({ data: [projectB], error: undefined })
    getProject.mockResolvedValue({
      data: undefined,
      error: { status: 403, diagnosticCode: 'LW_ACCESS_FORBIDDEN', detail: 'denied', requestId: 'req-b', retryable: false, title: '', type: 'about:blank', instance: '/' },
    })
    await auth.loadUser()
    await flushPromises()

    expect(wrapper.find('[data-testid="draft"]').exists()).toBe(false)
    expect(mounts).toBe(1)
    const recovery = wrapper.get('[data-testid="project-context-recovery"]')
    expect(recovery.text()).toContain('项目不存在或你无权访问。')
    expect(recovery.text()).toContain('切换到「项目 B」')
  })
})
