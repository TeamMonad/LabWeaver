<template>
  <div class="home-view">
    <section class="hero">
      <h1>欢迎进入 LabWeaver</h1>
      <p class="hero-subtitle">面向教学实验与科研工作的 Agent 驱动云原生实验平台</p>
    </section>

    <DiagnosticBanner
      v-if="routeReason"
      class="route-reason"
      :code="routeReason.code"
      :message="routeReason.message"
      :retryable="false"
      severity="warning"
    />

    <section v-if="!auth.isLoading.value" class="dashboard" aria-labelledby="task-heading">
      <h2 id="task-heading" class="section-title">
        {{ auth.isAuthenticated.value ? '可用任务' : '登录 LabWeaver' }}
      </h2>

      <div v-if="auth.isAuthenticated.value && visibleGroups.length" class="task-groups">
        <section
          v-for="group in visibleGroups"
          :key="group.id"
          class="task-group"
          :data-task-group="group.id"
          :aria-labelledby="`task-group-${group.id}`"
        >
          <div class="group-heading">
            <h3 :id="`task-group-${group.id}`">{{ group.label }}</h3>
            <span class="group-count">{{ group.items.length }} 项任务</span>
          </div>
          <div class="task-grid">
            <RouterLink
              v-for="item in group.items"
              :key="item.id"
              :to="navigationTarget(item, projects.selectedProjectId)"
              class="task-card md-card"
            >
              <div class="card-header">
                <span class="card-icon">
                  <SvgIcon :name="item.icon" size="lg" aria-hidden="true" />
                </span>
                <SvgIcon name="arrow_forward" size="md" aria-hidden="true" />
              </div>
              <h4 class="card-title">{{ item.label }}</h4>
              <p class="card-desc">{{ item.description }}</p>
            </RouterLink>
          </div>
        </section>
      </div>

      <div v-else-if="auth.isAuthenticated.value" class="empty-state">
        <SvgIcon name="block" size="lg" aria-hidden="true" />
        <p>当前账号未授予任何可用任务，请联系管理员。</p>
      </div>

      <div v-else class="empty-state">
        <SvgIcon name="login" size="lg" aria-hidden="true" />
        <p>请使用组织账号登录后查看可用任务。</p>
        <button v-if="oidcEnabled" type="button" class="filled-button" @click="auth.login()">
          <SvgIcon name="login" size="sm" aria-hidden="true" />
          <span>登录</span>
        </button>
        <p v-else class="hint">当前部署未配置 OIDC 登录服务。</p>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import SvgIcon from '@/components/common/SvgIcon.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import { useAuth } from '@/composables/useAuth'
import { useProjects } from '@/composables/useProjects'
import { OIDC_ENABLED } from '@/config'
import { navigationGroupsForRoles, navigationTarget, rolesFromProfile } from '@/utils/navigation'

const auth = useAuth()
const projects = useProjects()
const route = useRoute()
const oidcEnabled = OIDC_ENABLED

const visibleGroups = computed(() => {
  if (!auth.isAuthenticated.value) return []
  return navigationGroupsForRoles(rolesFromProfile(auth.user.value?.profile))
})

const routeReason = computed(() => {
  const reason = route.query.reason
  if (!reason || Array.isArray(reason)) return null
  switch (reason) {
    case 'unauthorized':
      return { code: 'unauthorized', message: '当前账号没有该页面的访问权限。' }
    case 'auth-not-configured':
      return { code: 'auth-not-configured', message: '当前部署未配置身份验证服务，无法访问受保护页面。' }
    case 'callback-failed':
      return { code: 'callback-failed', message: '登录回调处理失败，请重试。' }
    default:
      return { code: 'access-denied', message: '无法访问该页面。' }
  }
})
</script>

<style scoped>
.home-view {
  display: flex;
  flex-direction: column;
  gap: 32px;
  max-width: var(--content-max-width);
  margin: 0 auto;
}

.hero {
  padding: 24px 0 8px;
}

.hero h1 {
  margin-bottom: 8px;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-headline-large);
}

.hero-subtitle {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-large);
}

.section-title {
  margin-bottom: 20px;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-medium);
}

.task-groups {
  display: grid;
  gap: 28px;
}

.task-group {
  display: grid;
  gap: 12px;
}

.group-heading {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
}

.group-heading h3 {
  margin: 0;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-medium);
}

.group-count {
  flex-shrink: 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-medium);
}

.task-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
  gap: 16px;
}

.task-card {
  display: flex;
  min-height: 154px;
  flex-direction: column;
  padding: 20px;
  color: var(--md-sys-color-on-surface);
  text-decoration: none;
  transition: box-shadow 0.2s ease, transform 0.2s ease;
}

.task-card:hover {
  box-shadow: var(--md-sys-elevation-2);
  transform: translateY(-2px);
}

.task-card:focus-visible {
  outline: 2px solid var(--md-sys-color-primary);
  outline-offset: 2px;
}

.card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 18px;
  color: var(--md-sys-color-on-surface-variant);
}

.card-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 44px;
  height: 44px;
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
}

.card-title {
  margin-bottom: 6px;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-medium);
}

.card-desc {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
  line-height: 1.45;
}

.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 16px;
  padding: 48px 24px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-large);
  background: var(--md-sys-color-surface-container-high);
  color: var(--md-sys-color-on-surface-variant);
  text-align: center;
}

.filled-button {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  height: 40px;
  padding: 0 24px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.hint {
  font: var(--md-sys-body-small);
}

.dashboard {
  width: 100%;
}

.route-reason {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 16px;
  margin-bottom: 16px;
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-error-container);
  color: var(--md-sys-color-on-error-container);
  font: var(--md-sys-body-medium);
}

@media (max-width: 599px) {
  .task-grid {
    grid-template-columns: 1fr;
  }

  .group-heading {
    align-items: flex-start;
    flex-direction: column;
    gap: 4px;
  }
}

@media (prefers-reduced-motion: reduce) {
  .task-card {
    transition: none;
  }

  .task-card:hover {
    transform: none;
  }
}
</style>
