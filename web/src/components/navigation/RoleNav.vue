<template>
  <nav class="role-nav" aria-label="角色导航">
    <RouterLink
      v-for="item in navItems"
      :key="item.name"
      :to="item.path"
      class="nav-link"
      active-class="nav-link-active"
      @click="roleStore.setRole(item.role)"
    >
      <span class="nav-icon">{{ item.icon }}</span>
      <span class="nav-label">{{ item.label }}</span>
    </RouterLink>
  </nav>
</template>

<script setup lang="ts">
import { RouterLink } from 'vue-router'
import { useRoleStore } from '@/stores/role'

const roleStore = useRoleStore()

const navItems = [
  { name: 'teacher', role: 'teacher' as const, path: '/teacher', label: '教师', icon: '👨‍🏫' },
  { name: 'student', role: 'student' as const, path: '/student', label: '学生', icon: '🎓' },
  { name: 'researcher', role: 'researcher' as const, path: '/researcher', label: '科研', icon: '🔬' },
  { name: 'admin', role: 'admin' as const, path: '/admin', label: '管理', icon: '⚙️' },
]
</script>

<style scoped>
.role-nav {
  display: flex;
  gap: 8px;
}

.nav-link {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 8px 16px;
  border-radius: 6px;
  text-decoration: none;
  color: #606266;
  font-size: 14px;
  font-weight: 500;
  transition: all 0.2s;
}

.nav-link:hover {
  background: #f5f7fa;
  color: #409eff;
}

.nav-link-active {
  background: #ecf5ff;
  color: #409eff;
}

.nav-icon {
  font-size: 16px;
}
</style>
