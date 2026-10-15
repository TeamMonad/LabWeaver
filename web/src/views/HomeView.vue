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
              <span class="card-icon">
                <SvgIcon :name="item.icon" size="md" aria-hidden="true" />
              </span>
              <h4 class="card-title">{{ item.label }}</h4>
              <SvgIcon class="card-arrow" name="arrow_forward" size="md" aria-hidden="true" />
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
  gap: var(--space-8);
  max-width: var(--content-max-width);
  margin: 0 auto;
}

.hero {
  padding: var(--space-4) 0 0;
}

.hero h1 {
  margin-bottom: var(--space-2);
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-display-small);
  letter-spacing: -0.02em;
}

.hero-subtitle {
  max-width: 60ch;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-large);
}

.section-title {
  margin-bottom: var(--space-4);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-medium);
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.task-groups {
  display: grid;
  gap: var(--space-10);
}

.task-group {
  display: grid;
  gap: var(--space-4);
}

.group-heading {
  display: flex;
  align-items: baseline;
  gap: var(--space-3);
  padding-bottom: var(--space-3);
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.group-heading h3 {
  margin: 0;
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-large);
}

.group-count {
  flex-shrink: 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-medium);
  font-variant-numeric: tabular-nums;
}

.task-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(min(100%, 300px), 1fr));
  gap: var(--space-3);
}

.task-card {
  display: grid;
  grid-template-columns: 40px 1fr 20px;
  grid-template-rows: auto 1fr;
  column-gap: var(--space-4);
  row-gap: var(--space-1);
  align-items: center;
  padding: var(--space-4) var(--space-5);
  color: var(--md-sys-color-on-surface);
  text-decoration: none;
  box-shadow: none;
  transition: box-shadow var(--motion-duration-base) var(--motion-ease-out), transform var(--motion-duration-base) var(--motion-ease-out), border-color var(--motion-duration-base) var(--motion-ease-out);
}

.task-card:hover {
  border-color: color-mix(in srgb, var(--md-sys-color-primary) 32%, var(--md-sys-color-outline-variant));
  box-shadow: var(--md-sys-elevation-2);
  transform: translateY(-1px);
}

.task-card:active {
  transform: scale(0.99);
}

.task-card:focus-visible {
  outline: 2px solid var(--md-sys-color-primary);
  outline-offset: 2px;
}

.card-icon {
  grid-row: 1 / span 2;
  align-self: start;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 40px;
  height: 40px;
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container);
  color: var(--md-sys-color-primary);
  transition: background-color var(--motion-duration-base) var(--motion-ease-out);
}

.task-card:hover .card-icon {
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
}

.card-title {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-medium);
}

.card-arrow {
  color: var(--md-sys-color-on-surface-variant);
  opacity: 0.5;
  transition: transform var(--motion-duration-base) var(--motion-ease-out), opacity var(--motion-duration-base) var(--motion-ease-out);
}

.task-card:hover .card-arrow {
  opacity: 1;
  color: var(--md-sys-color-primary);
  transform: translateX(2px);
}

.card-desc {
  grid-column: 2 / span 2;
  align-self: start;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
}

.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-4);
  padding: var(--space-12) var(--space-6);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-large);
  background: var(--md-sys-color-surface-container-high);
  color: var(--md-sys-color-on-surface-variant);
  text-align: center;
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
  gap: var(--space-3);
  padding: var(--space-3) var(--space-4);
  margin-bottom: var(--space-4);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-error-container);
  color: var(--md-sys-color-on-error-container);
  font: var(--md-sys-body-medium);
}

@media (max-width: 599px) {
  .task-grid {
    grid-template-columns: 1fr;
  }

  .hero h1 {
    font: var(--md-sys-headline-medium);
  }

  .task-groups {
    gap: var(--space-8);
  }

  .task-grid {
    gap: 0;
    overflow: hidden;
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-large);
    background: var(--md-sys-color-surface);
  }

  .task-card {
    border: 0;
    border-radius: 0;
    box-shadow: none;
    padding: var(--space-3) var(--space-4);
  }

  .task-card + .task-card {
    border-top: 1px solid var(--md-sys-color-outline-variant);
  }

  .task-card:hover {
    box-shadow: none;
    transform: none;
  }
}

@media (prefers-reduced-motion: reduce) {
  .task-card {
    transition: none;
  }

  .task-card:hover,
  .task-card:hover .card-arrow {
    transform: none;
  }
}
</style>
