<template>
  <div class="home-view">
    <h1>LabWeaver 智织实验云</h1>
    <p class="subtitle">面向教学实验与科研工作的 Agent 驱动云原生实验平台</p>
    <div class="role-cards">
      <RouterLink
        v-for="card in roleCards"
        :key="card.role"
        :to="card.path"
        class="role-card"
        @click="roleStore.setRole(card.role)"
      >
        <span class="card-icon">{{ card.icon }}</span>
        <h3>{{ card.title }}</h3>
        <p>{{ card.desc }}</p>
      </RouterLink>
    </div>
  </div>
</template>

<script setup lang="ts">
import { RouterLink } from 'vue-router'
import { useRoleStore } from '@/stores/role'

const roleStore = useRoleStore()

const roleCards = [
  { role: 'teacher' as const, path: '/teacher', title: '教师入口', desc: '创建实验、审核环境、查看结果', icon: '👨‍🏫' },
  { role: 'student' as const, path: '/student', title: '学生入口', desc: '启动实验、提交任务、查看反馈', icon: '🎓' },
  { role: 'researcher' as const, path: '/researcher', title: '科研入口', desc: '申请算力、配置环境、管理数据', icon: '🔬' },
  { role: 'admin' as const, path: '/admin', title: '管理入口', desc: '审批资源、维护策略、审计平台', icon: '⚙️' },
]
</script>

<style scoped>
.home-view {
  text-align: center;
  padding: 48px 0;
}

h1 {
  font-size: 36px;
  margin-bottom: 12px;
  color: #1a1a1a;
}

.subtitle {
  color: #606266;
  margin-bottom: 48px;
  font-size: 16px;
}

.role-cards {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  gap: 24px;
  max-width: 960px;
  margin: 0 auto;
}

.role-card {
  display: block;
  padding: 32px 24px;
  background: #ffffff;
  border-radius: 12px;
  text-decoration: none;
  color: #1a1a1a;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.06);
  transition: transform 0.2s, box-shadow 0.2s;
}

.role-card:hover {
  transform: translateY(-4px);
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.1);
}

.card-icon {
  font-size: 40px;
  display: block;
  margin-bottom: 16px;
}

.role-card h3 {
  font-size: 20px;
  margin-bottom: 8px;
}

.role-card p {
  color: #606266;
  font-size: 14px;
}
</style>
