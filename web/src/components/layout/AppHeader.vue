<template>
  <header class="app-header">
    <div class="brand">
      <RouterLink to="/" class="brand-link">
        <span class="brand-icon">🧬</span>
        <span class="brand-text">LabWeaver</span>
      </RouterLink>
    </div>
    <RoleNav />
    <div class="current-role">
      <span v-if="roleStore.isAuthenticated" class="role-badge">
        {{ roleStore.roleLabel }}
      </span>
      <button v-else class="login-btn" @click="roleStore.setRole('student')">
        登录
      </button>
    </div>
  </header>
</template>

<script setup lang="ts">
import { RouterLink } from 'vue-router'
import RoleNav from '@/components/navigation/RoleNav.vue'
import { useRoleStore } from '@/stores/role'

const roleStore = useRoleStore()
</script>

<style scoped>
.app-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 24px;
  height: 64px;
  background: #ffffff;
  border-bottom: 1px solid #e4e7ed;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.04);
}

.brand-link {
  display: flex;
  align-items: center;
  gap: 10px;
  text-decoration: none;
  color: #1a1a1a;
  font-size: 20px;
  font-weight: 700;
}

.brand-icon {
  font-size: 24px;
}

.current-role {
  min-width: 100px;
  text-align: right;
}

.role-badge {
  display: inline-block;
  padding: 6px 14px;
  border-radius: 16px;
  background: #ecf5ff;
  color: #409eff;
  font-size: 14px;
  font-weight: 500;
}

.login-btn {
  padding: 6px 16px;
  border: 1px solid #409eff;
  border-radius: 4px;
  background: #409eff;
  color: #fff;
  cursor: pointer;
  font-size: 14px;
}

.login-btn:hover {
  background: #66b1ff;
}
</style>
