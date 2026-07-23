<template>
  <header class="top-app-bar">
    <div class="top-app-bar__leading">
      <button
        type="button"
        class="icon-button"
        aria-label="打开导航"
        @click="$emit('toggleDrawer')"
      >
        <SvgIcon name="menu" size="md" aria-label="打开导航" />
      </button>
      <RouterLink to="/" class="brand-link">
        <span class="brand-name">LabWeaver</span>
        <span class="brand-subtitle">智织实验云</span>
      </RouterLink>
    </div>

    <div class="top-app-bar__context">
      <span class="context-label" aria-label="当前课程上下文">课程 / 项目</span>
      <button
        type="button"
        class="context-button"
        aria-label="选择课程或项目上下文（待 #47 接入）"
        disabled
      >
        <SvgIcon name="folder_open" size="sm" aria-hidden="true" />
        <span class="context-button__text">选择上下文</span>
        <SvgIcon name="expand_more" size="sm" aria-hidden="true" />
      </button>
    </div>

    <div class="top-app-bar__trailing">
      <button
        type="button"
        class="icon-button"
        :aria-label="`切换主题，当前：${themeStore.preference}`"
        @click="cycleTheme"
      >
        <SvgIcon :name="themeIcon" size="md" :aria-label="`主题：${themeStore.preference}`" />
      </button>

      <template v-if="auth.isLoading.value">
        <span class="login-status" role="status">加载中…</span>
      </template>

      <template v-else-if="auth.isAuthenticated.value">
        <span class="user-chip" :title="userDisplayName">
          <SvgIcon name="person" size="sm" aria-hidden="true" />
          <span class="user-name">{{ userDisplayName }}</span>
        </span>
        <button type="button" class="text-button" aria-label="退出" @click="logout">
          <SvgIcon name="logout" size="sm" aria-hidden="true" />
          <span class="text-button__label">退出</span>
        </button>
      </template>

      <template v-else>
        <span class="login-status">未认证</span>
        <button v-if="oidcEnabled" type="button" class="filled-button" aria-label="登录" @click="login">
          <SvgIcon name="login" size="sm" aria-hidden="true" />
          <span class="filled-button__label">登录</span>
        </button>
      </template>
    </div>
  </header>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { RouterLink } from 'vue-router'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useAuth } from '@/composables/useAuth'
import { useThemeStore } from '@/stores/theme'
import { OIDC_ENABLED } from '@/config'

defineProps<{
  drawerOpen: boolean
}>()

defineEmits<{
  toggleDrawer: []
}>()

const auth = useAuth()
const themeStore = useThemeStore()
const oidcEnabled = OIDC_ENABLED

const userDisplayName = computed(() => {
  const profile = auth.user.value?.profile
  return (profile?.name as string) || (profile?.preferred_username as string) || (profile?.email as string) || '已登录用户'
})

const themeIcon = computed(() => {
  switch (themeStore.preference) {
    case 'light':
      return 'light_mode'
    case 'dark':
      return 'dark_mode'
    default:
      return 'brightness_auto'
  }
})

async function login() {
  await auth.login()
}

async function logout() {
  await auth.logout()
}

function cycleTheme() {
  const order: Array<'system' | 'light' | 'dark'> = ['system', 'light', 'dark']
  const next = order[(order.indexOf(themeStore.preference) + 1) % order.length]
  themeStore.setTheme(next)
}
</script>

<style scoped>
.top-app-bar {
  position: sticky;
  top: 0;
  z-index: 1100;
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: var(--app-top-bar-height);
  padding: 0 16px 0 4px;
  background: var(--md-sys-color-surface);
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
  box-shadow: var(--md-sys-elevation-1);
  gap: 16px;
}

.top-app-bar__leading,
.top-app-bar__trailing {
  display: flex;
  align-items: center;
  gap: 8px;
}

.top-app-bar__leading {
  min-width: 0;
}

.top-app-bar__trailing {
  flex-shrink: 0;
}

.top-app-bar__context {
  display: none;
  align-items: center;
  gap: 12px;
  flex: 1;
  min-width: 0;
}

.context-label {
  font: var(--md-sys-label-large);
  color: var(--md-sys-color-on-surface-variant);
  white-space: nowrap;
}

.context-button {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  height: 36px;
  padding: 0 12px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-surface-container);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-label-large);
  cursor: not-allowed;
  opacity: 0.7;
  white-space: nowrap;
}

.context-button__text {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 160px;
}

.brand-link {
  display: flex;
  flex-direction: column;
  min-width: 0;
  overflow: hidden;
  line-height: 1.2;
  text-decoration: none;
  color: var(--md-sys-color-on-surface);
}

.brand-name {
  font: var(--md-sys-title-medium);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.brand-subtitle {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.icon-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 48px;
  height: 48px;
  padding: 0;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  cursor: pointer;
}

.icon-button:hover {
  background: var(--md-sys-color-surface-container-highest);
}

.login-status,
.user-name {
  font: var(--md-sys-label-large);
  color: var(--md-sys-color-on-surface-variant);
}

.user-chip {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  max-width: 120px;
  height: 32px;
  padding: 0 12px;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
  overflow: hidden;
}

.user-name {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.text-button,
.filled-button {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 12px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  font: var(--md-sys-label-large);
  cursor: pointer;
  white-space: nowrap;
}

.text-button {
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
}

.text-button:hover {
  background: var(--md-sys-color-surface-container-highest);
}

.filled-button {
  padding: 0 16px;
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
}

.filled-button:hover {
  background: var(--md-sys-color-on-primary-container);
}

@media (min-width: 840px) {
  .top-app-bar {
    padding: 0 24px 0 16px;
  }

  .top-app-bar__context {
    display: flex;
  }
}

@media (min-width: 1200px) {
  .user-chip {
    max-width: 200px;
  }

  .context-button__text {
    max-width: 240px;
  }
}

@media (max-width: 599px) {
  .brand-subtitle,
  .user-name,
  .text-button__label,
  .filled-button__label {
    display: none;
  }

  .user-chip {
    justify-content: center;
    width: 32px;
    min-width: 32px;
    padding: 0;
  }
}
</style>
