<template>
  <div class="auth-error-view">
    <section class="error-card md-card" role="alert">
      <h1>{{ title }}</h1>
      <p>{{ message }}</p>
      <div class="error-actions">
        <button v-if="canLogin" type="button" class="primary-button" @click="login">登录</button>
        <RouterLink to="/" class="secondary-button">返回首页</RouterLink>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { RouterLink } from 'vue-router'
import { OIDC_ENABLED } from '@/config'
import { useAuth } from '@/composables/useAuth'

const props = defineProps<{
  reason?: string
}>()

const auth = useAuth()

const canLogin = computed(() => OIDC_ENABLED)

const title = computed(() => {
  switch (props.reason) {
    case 'session_required':
      return '需要登录'
    case 'session_expired':
      return '会话已过期'
    case 'role_denied':
      return '无权访问该角色页面'
    case 'oidc_not_configured':
      return '身份认证未配置'
    default:
      return '认证失败'
  }
})

const message = computed(() => {
  switch (props.reason) {
    case 'session_required':
      return '该页面需要登录后才能访问。'
    case 'session_expired':
      return '你的登录会话已过期，请重新登录。'
    case 'role_denied':
      return '当前账号没有访问该角色工作台的权限。'
    case 'oidc_not_configured':
      return '当前部署未配置 OIDC Provider，无法完成身份认证。请联系管理员。'
    default:
      return '登录过程中发生错误，请返回首页重试。'
  }
})

async function login() {
  await auth.login()
}
</script>

<style scoped>
.auth-error-view {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 60vh;
}

.error-card {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 16px;
  max-width: 520px;
  padding: 40px 48px;
  text-align: center;
}

.error-card h1 {
  font: var(--md-sys-headline-small);
  color: var(--md-sys-color-error);
}

.error-card p {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.error-actions {
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
</style>
