/**
 * Browser harness for cross-tab BFF session switch through the real useAuth +
 * apiClient path (same pieces App.vue wires: sessionEpoch remount + write gate).
 *
 * Kept minimal (no AppShell/router) so Playwright can mock BFF only; production
 * App.vue uses the same sessionEpoch remount contract.
 */
import { createApp, defineComponent, onMounted, ref, computed, h } from 'vue'
import { initializeSdkClient, apiClient } from '@/api/client'
import { useAuth } from '@/composables/useAuth'

const SessionForm = defineComponent({
  name: 'SessionForm',
  setup() {
    const note = ref('')
    const lastResult = ref('')
    const submitting = ref(false)

    async function submit() {
      submitting.value = true
      lastResult.value = ''
      try {
        const response = await apiClient.post({
          url: '/api/v1/resource-requests',
          body: { note: note.value },
          throwOnError: true,
        })
        lastResult.value = `ok:${JSON.stringify(response.data ?? null)}`
      } catch (error: unknown) {
        const err = error as {
          diagnosticCode?: string
          response?: { status?: number; data?: { diagnosticCode?: string } }
          message?: string
        }
        const code =
          err?.diagnosticCode
          ?? err?.response?.data?.diagnosticCode
          ?? err?.message
          ?? 'unknown'
        lastResult.value = `error:${code}`
      } finally {
        submitting.value = false
      }
    }

    return () =>
      h('section', { 'data-testid': 'session-form' }, [
        h('label', { for: 'note-input' }, 'Note'),
        h('input', {
          id: 'note-input',
          'data-testid': 'note-input',
          value: note.value,
          onInput: (event: Event) => {
            note.value = (event.target as HTMLInputElement).value
          },
        }),
        h(
          'button',
          {
            type: 'button',
            'data-testid': 'submit-note',
            disabled: submitting.value,
            onClick: () => {
              void submit()
            },
          },
          'Submit',
        ),
        h('p', { 'data-testid': 'submit-result', role: 'status' }, lastResult.value),
      ])
  },
})

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
        // Mirrors App.vue: <RouterView :key="sessionEpoch" /> so pending form state
        // cannot survive a CSRF-driven actor switch.
        h(SessionForm, { key: auth.sessionEpoch.value }),
      ])
  },
})

async function bootstrap() {
  await initializeSdkClient()
  createApp(HarnessApp).mount('#app')
}

void bootstrap()
