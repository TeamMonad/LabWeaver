import { onScopeDispose, watch, type Ref } from 'vue'

/**
 * Calls `tick` while `active` is true and the tab is visible. The delay grows
 * from `initialMs` to `maxMs` and resets whenever polling restarts. Hidden tabs
 * and finished work stop polling entirely.
 */
export function useVisiblePolling(tick: () => Promise<unknown> | unknown, active: Ref<boolean>, initialMs = 10_000, maxMs = 30_000) {
  let timer: ReturnType<typeof setTimeout> | null = null
  let delay = initialMs

  function visible() {
    return typeof document === 'undefined' || document.visibilityState !== 'hidden'
  }

  function stop() {
    if (timer !== null) clearTimeout(timer)
    timer = null
  }

  function schedule() {
    stop()
    if (!active.value || !visible()) return
    timer = setTimeout(async () => {
      timer = null
      try {
        await tick()
      } finally {
        delay = Math.min(Math.round(delay * 1.5), maxMs)
        schedule()
      }
    }, delay)
  }

  function restart() {
    delay = initialMs
    schedule()
  }

  function onVisibility() {
    if (!visible()) {
      stop()
      return
    }
    if (!active.value) return
    // Returning to the tab reads once right away, then resumes the slow cadence.
    delay = initialMs
    void Promise.resolve(tick()).finally(schedule)
  }

  if (typeof document !== 'undefined') document.addEventListener('visibilitychange', onVisibility)
  watch(active, (value) => (value ? restart() : stop()), { immediate: true })
  onScopeDispose(() => {
    stop()
    if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', onVisibility)
  })

  return { restart, stop }
}
