<template>
  <nav class="role-nav" aria-label="角色导航">
    <RouterLink
      v-for="item in navItems"
      :key="item.name"
      :to="item.path"
      class="nav-pill"
      active-class="nav-pill-active"
      @click="roleStore.setRole(item.role)"
    >
      <span class="nav-icon" aria-hidden="true">{{ item.icon }}</span>
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
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 4px;
  background: var(--md-sys-color-surface-variant);
  border-radius: var(--md-sys-shape-full);
}

.nav-pill {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  height: 40px;
  padding: 0 20px;
  border-radius: var(--md-sys-shape-full);
  text-decoration: none;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-large);
  transition: background 0.2s, color 0.2s;
}

.nav-pill:hover {
  background: rgba(32, 33, 36, 0.04);
  color: var(--md-sys-color-on-surface);
}

.nav-pill-active {
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  box-shadow: var(--md-sys-elevation-1);
}

.nav-pill-active:hover {
  background: var(--md-sys-color-primary);
}

.nav-icon {
  font-size: 16px;
}
</style>
