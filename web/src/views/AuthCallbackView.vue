<template>
  <div class="callback-view">
    <div v-if="auth.isLoading.value" class="callback-card md-card" role="status">
      <span class="spinner" aria-hidden="true" />
      <p>正在完成登录，请稍候…</p>
    </div>

    <div v-else-if="auth.error.value" class="callback-card md-card callback-error" role="alert">
      <h1>登录失败</h1>
      <p class="error-detail">{{ auth.error.value.message }}</p>
      <div class="callback-actions">
        <RouterLink to="/" class="primary-button">返回首页</RouterLink>
        <button type="button" class="secondary-button" @click="retryLogin">重新登录</button>
      </div>
    </div>

    <div v-else-if="auth.isAuthenticated.value" class="callback-card md-card callback-success" role="status">
      <h1>登录成功</h1>
      <p>正在跳转…</p>
    </div>

    <div v-else class="callback-card md-card callback-error" role="alert">
      <h1>登录未完成</h1>
      <p>未获取到有效的身份信息。</p>
      <div class="callback-actions">
        <RouterLink to="/" class="primary-button">返回首页</RouterLink>
        <button type="button" class="secondary-button" @click="retryLogin">重新登录</button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, watch } from 'vue'
import { RouterLink, useRouter } from 'vue-router'
import { useAuth } from '@/composables/useAuth'

const router = useRouter()
const auth = useAuth()

async function retryLogin() {
  await auth.login()
}

onMounted(async () => {
  await auth.handleCallback()
})

watch(
  () => auth.isAuthenticated.value,
  (authenticated) => {
    if (authenticated) {
      // Redirect to the originally requested path or to the home page.
      const returnTo = window.sessionStorage.getItem('auth-return-to')
      window.sessionStorage.removeItem('auth-return-to')
      router.replace(returnTo || '/')
    }
  },
  { immediate: true }
)
</script>

<style scoped>
.callback-view {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 60vh;
}

.callback-card {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 16px;
  padding: 40px 48px;
  text-align: center;
}

.callback-card h1 {
  font: var(--md-sys-headline-small);
}

.callback-card p {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.error-detail {
  max-width: 480px;
  word-break: break-word;
  color: var(--md-sys-color-error);
}

.callback-actions {
  display: flex;
  gap: 12px;
  margin-top: 8px;
}

.primary-button,
.secondary-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  height: 40px;
  padding: 0 24px;
  border-radius: var(--md-sys-shape-full);
  font: var(--md-sys-label-large);
  text-decoration: none;
  cursor: pointer;
}

.primary-button {
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  border: none;
}

.secondary-button {
  background: transparent;
  color: var(--md-sys-color-primary);
  border: 1px solid var(--md-sys-color-outline);
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
