<template>
  <div class="home-view">
    <section class="hero">
      <h1>欢迎进入 LabWeaver</h1>
      <p class="hero-subtitle">面向教学实验与科研工作的 Agent 驱动云原生实验平台</p>
    </section>

    <section class="dashboard">
      <h2 class="section-title">选择角色入口</h2>
      <div class="role-grid">
        <RouterLink
          v-for="card in roleCards"
          :key="card.role"
          :to="card.path"
          class="role-card md-card"
          @click="roleStore.setRole(card.role)"
        >
          <div class="card-header">
            <span class="card-icon" :style="{ background: card.tone }">{{ card.icon }}</span>
            <span class="card-arrow" aria-hidden="true">→</span>
          </div>
          <h3 class="card-title">{{ card.title }}</h3>
          <p class="card-desc">{{ card.desc }}</p>
        </RouterLink>
      </div>
    </section>

    <section class="status-bar md-card">
      <div class="status-item">
        <span class="status-dot" :style="{ background: '#34a853' }" />
        <span class="status-label">平台服务</span>
        <span class="status-value">运行中</span>
      </div>
      <div class="status-divider" />
      <div class="status-item">
        <span class="status-dot" :style="{ background: '#fbbc04' }" />
        <span class="status-label">项目阶段</span>
        <span class="status-value">Sprint 1 Foundation</span>
      </div>
      <div class="status-divider" />
      <div class="status-item">
        <span class="status-label">版本</span>
        <span class="status-value">v0.1.0-dev</span>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { RouterLink } from 'vue-router'
import { useRoleStore } from '@/stores/role'

const roleStore = useRoleStore()

const roleCards = [
  {
    role: 'teacher' as const,
    path: '/teacher',
    title: '教师入口',
    desc: '创建实验、审核环境、查看结果',
    icon: '👨‍🏫',
    tone: '#d3e3fd',
  },
  {
    role: 'student' as const,
    path: '/student',
    title: '学生入口',
    desc: '启动实验、提交任务、查看反馈',
    icon: '🎓',
    tone: '#e6f4ea',
  },
  {
    role: 'researcher' as const,
    path: '/researcher',
    title: '科研入口',
    desc: '申请算力、配置环境、管理数据',
    icon: '🔬',
    tone: '#fce8e6',
  },
  {
    role: 'admin' as const,
    path: '/admin',
    title: '管理入口',
    desc: '审批资源、维护策略、审计平台',
    icon: '⚙️',
    tone: '#f3e8fd',
  },
]
</script>

<style scoped>
.home-view {
  display: flex;
  flex-direction: column;
  gap: 32px;
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
  transition: box-shadow 0.2s, transform 0.2s;
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
  font-size: 24px;
}

.card-arrow {
  color: var(--md-sys-color-on-surface-variant);
  font-size: 20px;
}

.card-title {
  font: var(--md-sys-title-medium);
  margin-bottom: 8px;
}

.card-desc {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
}

.status-bar {
  display: flex;
  align-items: center;
  gap: 24px;
  padding: 16px 24px;
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
</style>
