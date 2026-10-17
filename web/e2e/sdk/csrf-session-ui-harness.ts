/**
 * Browser harness for cross-tab BFF session switch through the real useAuth +
 * apiClient path (same pieces App.vue wires: sessionEpoch remount + write gate).
 *
 * Kept minimal (no AppShell/router) so Playwright can mock BFF only; production
 * App.vue uses the same sessionEpoch remount contract via <RouterView :key>.
 */
import { createApp, defineComponent, onMounted, computed, h } from 'vue'
import { initializeSdkClient } from '@/api/client'
import { useAuth } from '@/composables/useAuth'
import { SessionNoteForm } from './csrf-session-ui-form'

const HarnessApp = defineComponent({
  name: 'CsrfSessionUiHarness',
  setup() {
    const auth = useAuth()
    const actorId = computed(() => {
      const profile = auth.user.value?.profile as { actor_id?: string } | undefined
      return profile?.actor_id ?? ''
    })

    onMounted(() => {
      void auth.loadUser()
    })

    async function switchToActorB() {
      await fetch('/e2e/mock-login-b', { method: 'POST', credentials: 'include' })
      await auth.loadUser()
    }

    return () =>
      h('div', { 'data-testid': 'csrf-session-harness' }, [
        h('header', { 'data-testid': 'session-header' }, [
          h('span', { 'data-testid': 'session-actor' }, actorId.value || 'none'),
          h('span', { 'data-testid': 'session-epoch' }, String(auth.sessionEpoch.value)),
          h(
            'button',
            {
              type: 'button',
              'data-testid': 'switch-to-b',
              onClick: () => {
                void switchToActorB()
              },
            },
            'Switch shared session to B',
          ),
        ]),
        // Mirrors App.vue: <RouterView :key="sessionEpoch" />
        h(SessionNoteForm, { key: auth.sessionEpoch.value }),
      ])
  },
})

async function bootstrap() {
  await initializeSdkClient()
  createApp(HarnessApp).mount('#app')
}

void bootstrap()
