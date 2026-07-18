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

    <section v-if="!auth.isLoading.value" class="dashboard" aria-labelledby="role-heading">
      <h2 id="role-heading" class="section-title">{{ sectionTitle }}</h2>

      <div v-if="auth.isAuthenticated.value && visibleCards.length" class="role-grid">
        <RouterLink
          v-for="card in visibleCards"
          :key="card.role"
          :to="card.path"
          class="role-card md-card"
        >
          <div class="card-header">
            <span class="card-icon" :style="{ background: card.tone }">
              <SvgIcon :name="card.icon" size="lg" />
            </span>
            <SvgIcon name="arrow_forward" size="md" aria-hidden="true" />
          </div>
          <h3 class="card-title">{{ card.title }}</h3>
          <p class="card-desc">{{ card.desc }}</p>
        </RouterLink>
      </div>

      <div v-else-if="auth.isAuthenticated.value && !visibleCards.length" class="empty-state">
        <SvgIcon name="block" size="lg" aria-hidden="true" />
        <p>当前账号未分配任何角色入口，请联系管理员。</p>
      </div>

      <div v-else class="empty-state">
        <SvgIcon name="login" size="lg" aria-hidden="true" />
        <p>请使用组织账号登录后查看授权的角色入口。</p>
        <template v-if="fixtureDemoRoles.length > 0">
          <p class="fixture-demo-hint">Fixture 演示：选择一个确定性身份直接进入受保护页面</p>
          <div class="fixture-demo-roles">
            <button
              v-for="demoRole in fixtureDemoRoles"
              :key="demoRole.id"
              type="button"
              class="fixture-demo-role"
              @click="signInFixtureDemo(demoRole.id)"
            >
              <span class="fixture-demo-role__label">{{ demoRole.label }}</span>
              <span class="fixture-demo-role__desc">{{ demoRole.description }}</span>
            </button>
          </div>
        </template>
        <template v-else>
          <button v-if="oidcEnabled" type="button" class="filled-button" @click="auth.login()">
            <SvgIcon name="login" size="sm" aria-hidden="true" />
            <span>登录</span>
          </button>
          <p v-else class="hint">当前部署未配置 OIDC 登录服务。</p>
        </template>
      </div>
    </section>

    <section class="status-bar md-card">
      <div class="status-item">
        <span class="status-dot" :style="{ background: 'var(--md-sys-color-success)' }" />
        <span class="status-label">平台服务</span>
        <span class="status-value">运行中</span>
      </div>
      <div class="status-divider" />
      <div class="status-item">
        <span class="status-dot" :style="{ background: 'var(--md-sys-color-warning)' }" />
        <span class="status-label">项目阶段</span>
        <span class="status-value">Sprint 2 Environment</span>
      </div>
      <div class="status-divider" />
      <div class="status-item">
        <span class="status-label">版本</span>
        <span class="status-value">v0.2.0-dev</span>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import SvgIcon from '@/components/common/SvgIcon.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import { useAuth } from '@/composables/useAuth'
import { OIDC_ENABLED } from '@/config'
import type { AppRole } from '@/router'
import type { FixtureDemoRole } from '@/fixture/devAuth'

const auth = useAuth()
const route = useRoute()
const oidcEnabled = OIDC_ENABLED

// Fixture builds replace the OIDC login with deterministic demo identities so
// protected pages stay reachable for manual browser acceptance. The fixture
// module is loaded dynamically and never ships in the production bundle.
const fixtureDemoRoles = ref<FixtureDemoRole[]>([])
let fixtureSignIn: ((roleId: string) => void) | undefined

onMounted(async () => {
  if (__IS_FIXTURE__) {
    const mod = await import('@/fixture/devAuth')
    fixtureDemoRoles.value = mod.FIXTURE_DEMO_ROLES
    fixtureSignIn = mod.signInFixtureDemo
  }
})

function signInFixtureDemo(roleId: string) {
  const entry = fixtureDemoRoles.value.find((r) => r.id === roleId)
  if (!entry || !fixtureSignIn) return

  // If the user arrived here from a protected route, only return to that route
  // when the selected fixture role can actually enter it. Otherwise drop the
  // stored return path so the role lands on its own home page instead of a
  // role-denied error page.
  const effectiveRole: AppRole = entry.role === 'platform-admin' ? 'admin' : entry.role
  const roleCard = roleCards.find((c) => c.role === effectiveRole)
  const returnTo = window.sessionStorage.getItem('auth-return-to')
  if (returnTo && roleCard && !returnTo.startsWith(roleCard.path)) {
    window.sessionStorage.removeItem('auth-return-to')
  }

  fixtureSignIn(roleId)
}

interface RoleCard {
  role: AppRole
  path: string
  title: string
  desc: string
  icon: string
  tone: string
}

const roleCards: RoleCard[] = [
  {
    role: 'teacher',
    path: '/teacher',
    title: '教师入口',
    desc: '创建实验、审核环境、查看结果',
    icon: 'school',
    tone: 'var(--md-sys-color-primary-container)',
  },
  {
    role: 'student',
    path: '/student',
    title: '学生入口',
    desc: '启动实验、提交任务、查看反馈',
    icon: 'person',
    tone: 'var(--md-sys-color-secondary-container)',
  },
  {
    role: 'researcher',
    path: '/researcher',
    title: '科研入口',
    desc: '申请算力、配置环境、管理数据',
    icon: 'science',
    tone: 'var(--md-sys-color-tertiary-container)',
  },
  {
    role: 'admin',
    path: '/admin',
    title: '管理入口',
    desc: '审批资源、维护策略、审计平台',
    icon: 'admin_panel_settings',
    tone: 'var(--md-sys-color-surface-container-high)',
  },
]

function getUserRoles(): AppRole[] {
  const user = auth.user.value
  if (!user || user.expired) return []
  const roles = user.profile?.roles ?? user.profile?.role
  if (Array.isArray(roles)) return roles.filter((r): r is AppRole => roleCards.some((c) => c.role === r))
  if (typeof roles === 'string') {
    return roles
      .split(',')
      .map((r) => r.trim())
      .filter((r): r is AppRole => roleCards.some((c) => c.role === r))
  }
  return []
}

const visibleCards = computed(() => {
  if (!auth.isAuthenticated.value) return []
  const userRoles = getUserRoles()
  return roleCards.filter((card) => userRoles.includes(card.role))
})

const sectionTitle = computed(() => {
  if (!auth.isAuthenticated.value) return '登录 LabWeaver'
  return '选择角色入口'
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
  font: var(--md-sys-headline-large);
  color: var(--md-sys-color-on-surface);
  margin-bottom: 8px;
}

.hero-subtitle {
  font: var(--md-sys-body-large);
  color: var(--md-sys-color-on-surface-variant);
}

.section-title {
  font: var(--md-sys-title-medium);
  color: var(--md-sys-color-on-surface);
  margin-bottom: 16px;
}

.role-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
  gap: 20px;
}

.role-card {
  display: flex;
  flex-direction: column;
  padding: 24px;
  text-decoration: none;
  color: var(--md-sys-color-on-surface);
  transition: box-shadow 0.2s ease, transform 0.2s ease;
}

.role-card:hover {
  box-shadow: var(--md-sys-elevation-2);
  transform: translateY(-2px);
}

.card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 20px;
}

.card-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 48px;
  height: 48px;
  border-radius: var(--md-sys-shape-medium);
  color: var(--md-sys-color-on-surface);
}

.card-title {
  font: var(--md-sys-title-medium);
  margin-bottom: 8px;
}

.card-desc {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 16px;
  padding: 48px 24px;
  border-radius: var(--md-sys-shape-large);
  background: var(--md-sys-color-surface-container-high);
  border: 1px solid var(--md-sys-color-outline-variant);
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

.fixture-demo-hint {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.fixture-demo-roles {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
  max-width: 420px;
}

.fixture-demo-role {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  padding: 10px 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface);
  color: var(--md-sys-color-on-surface);
  cursor: pointer;
  text-align: left;
}

.fixture-demo-role:hover {
  background: var(--md-sys-color-surface-container-highest);
}

.fixture-demo-role__label {
  font: var(--md-sys-label-large);
}

.fixture-demo-role__desc {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.hint {
  font: var(--md-sys-body-small);
}

.status-bar {
  display: flex;
  align-items: center;
  gap: 24px;
  padding: 16px 24px;
  flex-wrap: wrap;
}

.status-item {
  display: flex;
  align-items: center;
  gap: 10px;
}

.status-dot {
  width: 10px;
  height: 10px;
  border-radius: var(--md-sys-shape-full);
}

.status-label {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.status-value {
  font: var(--md-sys-label-large);
  color: var(--md-sys-color-on-surface);
}

.status-divider {
  width: 1px;
  height: 24px;
  background: var(--md-sys-color-outline-variant);
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
  .role-grid {
    grid-template-columns: 1fr;
  }

  .status-bar {
    flex-direction: column;
    align-items: flex-start;
    gap: 12px;
  }

  .status-divider {
    display: none;
  }
}

@media (prefers-reduced-motion: reduce) {
  .role-card {
    transition: none;
  }

  .role-card:hover {
    transform: none;
  }
}
</style>
