<template>
  <div class="callback-view" role="status" aria-live="polite">
    <span class="spinner" aria-hidden="true" />
    <span>正在完成登录，请稍候…</span>
  </div>
</template>

<script setup lang="ts">
import { onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { useAuth } from '@/composables/useAuth'

const router = useRouter()
const auth = useAuth()

onMounted(async () => {
  await auth.handleCallback()
  if (auth.error.value) {
    await router.replace({ name: 'home', query: { reason: 'callback-failed' } })
    return
  }
  await router.replace({ name: 'home' })
})
</script>

<style scoped>
.callback-view {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 16px;
  min-height: 100vh;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
}

.spinner {
  width: 32px;
  height: 32px;
  border: 3px solid var(--md-sys-color-outline-variant);
  border-top-color: var(--md-sys-color-primary);
  border-radius: 50%;
  animation: spin 1s linear infinite;
}

@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

@media (prefers-reduced-motion: reduce) {
  .spinner {
    animation: none;
  }
}
</style>
