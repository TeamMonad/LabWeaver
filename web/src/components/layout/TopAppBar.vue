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
      <GcpProjectSelector class="top-project-selector" />
    </div>

    <div class="top-app-bar__center">
      <GcpSearchBar />
    </div>

    <div class="top-app-bar__trailing">
      <RouterLink
        v-if="consoleTarget"
        :to="consoleTarget"
        class="icon-button shell-button"
        title="打开云终端控制台 (Cloud Shell)"
        aria-label="打开云终端控制台"
      >
        <SvgIcon name="terminal" size="md" aria-hidden="true" />
      </RouterLink>

      <button
        type="button"
        class="icon-button"
        :aria-label="`切换主题，当前：${themeStore.preference}`"
        @click="cycleTheme"
      >
        <SvgIcon :name="themeIcon" size="md" :aria-label="`主题：${themeStore.preference}`" />
      </button>

      <span
        v-if="auth.error.value"
        class="login-error"
        role="alert"
      >
        认证操作失败：{{ auth.error.value.message }}
      </span>

      <template v-if="auth.isLoading.value">
        <span class="login-status" role="status">加载中…</span>
      </template>

      <template v-else-if="auth.isAuthenticated.value">
        <RouterLink
          :to="notificationCenterPath"
          class="icon-button notification-button"
          :aria-label="notificationLabel"
          :title="notificationLabel"
          data-testid="notification-entry"
        >
          <SvgIcon name="notifications" size="md" aria-hidden="true" />
          <span v-if="unreadText" class="notification-badge" aria-hidden="true" data-testid="notification-unread-badge">{{ unreadText }}</span>
        </RouterLink>
        <span class="user-chip" :title="userDisplayName">
          <SvgIcon name="person" size="sm" aria-hidden="true" />
          <span class="user-name">{{ userDisplayName }}</span>
        </span>
        <button type="button" class="text-button" aria-label="退出" @click="logout">
          <SvgIcon name="logout" size="sm" aria-hidden="true" />
          <span class="text-button__label">退出</span>
        </button>
      </template>

      <!-- The session may still exist while its identity cannot be read: keep a way to end it. -->
      <template v-else-if="auth.identityUnknown.value">
        <span class="login-status">账号未确认</span>
        <button type="button" class="text-button" aria-label="退出" data-testid="identity-unknown-logout" @click="endBrowserSession">
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
import { RouterLink, useRoute } from 'vue-router'
import SvgIcon from '@/components/common/SvgIcon.vue'
import GcpProjectSelector from '@/components/layout/GcpProjectSelector.vue'
import GcpSearchBar from '@/components/layout/GcpSearchBar.vue'
import { useAuth } from '@/composables/useAuth'
import { useProjects } from '@/composables/useProjects'
import { useUnreadNotificationCount } from '@/composables/useNotifications'
import { useThemeStore } from '@/stores/theme'
import { OIDC_ENABLED } from '@/config'
import { consoleNavigationTarget, NOTIFICATION_CENTER_PATH, rolesFromProfile } from '@/utils/navigation'
import { unreadBadgeText } from '@/utils/notifications'

defineProps<{
  drawerOpen: boolean
}>()

defineEmits<{
  toggleDrawer: []
}>()

const auth = useAuth()
const projects = useProjects()
const themeStore = useThemeStore()
const oidcEnabled = OIDC_ENABLED
const route = useRoute()

const consoleTarget = computed(() => consoleNavigationTarget(
  auth.isAuthenticated.value ? rolesFromProfile(auth.user.value?.profile) : [],
  route.path,
  projects.selectedProjectId,
))

const notificationCenterPath = NOTIFICATION_CENTER_PATH
const unread = useUnreadNotificationCount(computed(() => auth.isAuthenticated.value))
// Zero shows no badge; a count that cannot be read shows no number rather than a stale one.
const unreadText = computed(() => (unread.value.kind === 'success' && unread.value.data > 0 ? unreadBadgeText(unread.value.data) : ''))
const notificationLabel = computed(() => {
  if (unread.value.kind === 'success') return unread.value.data > 0 ? `通知，${unread.value.data} 条未读` : '通知，没有未读'
  if (unread.value.kind === 'error') return '通知，未读数暂时无法读取'
  return '通知'
})

const userDisplayName = computed(() => {
  const profile = auth.user.value?.profile
  if (!profile) return '已登录用户'
  const claims = profile as Record<string, unknown>
  return (typeof claims.name === 'string' && claims.name)
    || (typeof claims.preferred_username === 'string' && claims.preferred_username)
    || (typeof claims.email === 'string' && claims.email)
    || '已登录用户'
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

async function endBrowserSession() {
  await auth.endBrowserSession()
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
  padding: 0 var(--space-4) 0 var(--space-1);
  background: var(--md-sys-color-surface);
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
  box-shadow: var(--md-sys-elevation-1);
  gap: var(--space-4);
}

.top-app-bar__leading,
.top-app-bar__trailing {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  flex-shrink: 0;
}

.top-app-bar__leading {
  gap: var(--space-3);
}

.top-project-selector {
  margin-left: var(--space-2);
}

.top-app-bar__center {
  display: flex;
  align-items: center;
  justify-content: center;
  flex: 1;
  max-width: 540px;
  min-width: 0;
}

.shell-button {
  text-decoration: none;
}

.notification-button {
  position: relative;
  text-decoration: none;
}

.notification-badge {
  position: absolute;
  top: 2px;
  right: 0;
  min-width: 18px;
  height: 18px;
  padding: 0 4px;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-error);
  color: var(--md-sys-color-on-error);
  font: var(--md-sys-label-small);
  line-height: 18px;
  text-align: center;
  pointer-events: none;
}

.context-label {
  font: var(--md-sys-label-large);
  color: var(--md-sys-color-on-surface-variant);
  white-space: nowrap;
}

.context-button {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  height: 36px;
  padding: 0 var(--space-3);
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
  line-height: 1.2;
  text-decoration: none;
  color: var(--md-sys-color-on-surface);
}

.brand-name {
  font: var(--md-sys-title-medium);
}

.brand-subtitle {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.login-status,
.user-name {
  font: var(--md-sys-label-large);
  color: var(--md-sys-color-on-surface-variant);
}

.login-error {
  font: var(--md-sys-label-large);
  color: var(--md-sys-color-error);
}

.user-chip {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  max-width: 120px;
  height: 32px;
  padding: 0 var(--space-3);
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

@media (min-width: 840px) {
  .top-app-bar {
    padding: 0 var(--space-6) 0 var(--space-4);
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
  .top-app-bar {
    gap: var(--space-1);
    padding-right: var(--space-2);
  }

  .top-app-bar__leading,
  .top-app-bar__trailing {
    min-width: 0;
    gap: var(--space-1);
  }

  .top-app-bar__leading {
    flex: 1 1 auto;
  }

  .top-app-bar__trailing {
    flex: 0 1 auto;
  }

  .top-app-bar .top-project-selector,
  .top-app-bar .shell-button {
    display: none;
  }

  .brand-subtitle,
  .text-button__label,
  .filled-button__label {
    display: none;
  }
}

@media (max-width: 480px) {
  .top-app-bar__leading {
    overflow: hidden;
  }

  .brand-link,
  .brand-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .top-app-bar .user-chip,
  .top-app-bar .text-button,
  .top-app-bar .filled-button {
    flex: 0 0 36px;
    width: 36px;
    justify-content: center;
    padding-right: 0;
    padding-left: 0;
  }

  .top-app-bar .user-name,
  .top-app-bar .login-status {
    display: none;
  }
}
</style>
