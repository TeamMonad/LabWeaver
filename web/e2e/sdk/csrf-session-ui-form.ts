import { defineComponent, ref, h } from 'vue'
import { apiClient } from '@/api/client'

/** Form child remounted when the harness passes a new `key` (sessionEpoch). */
export const SessionNoteForm = defineComponent({
  name: 'SessionNoteForm',
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
