<template>
  <header class="app-header">
    <div class="header-left">
      <RouterLink to="/" class="brand-link">
        <span class="brand-logo" aria-hidden="true">
          <svg viewBox="0 0 24 24" width="28" height="28">
            <path
              fill="currentColor"
              d="M12 2L2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5"
            />
          </svg>
        </span>
        <div class="brand-text">
          <span class="brand-name">LabWeaver</span>
          <span class="brand-subtitle">智织实验云</span>
        </div>
      </RouterLink>
    </div>

    <div class="header-center">
      <RoleNav />
    </div>

    <div class="header-right">
      <span v-if="roleStore.currentRole" class="role-chip">
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
  position: sticky;
  top: 0;
  z-index: 100;
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: var(--app-header-height);
  padding: 0 24px;
  background: var(--md-sys-color-surface);
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
  box-shadow: var(--md-sys-elevation-1);
}

.header-left,
.header-center,
.header-right {
  display: flex;
  align-items: center;
  gap: 16px;
}

.brand-link {
  display: flex;
  align-items: center;
  gap: 12px;
  text-decoration: none;
  color: var(--md-sys-color-on-surface);
}

.brand-logo {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 36px;
  height: 36px;
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
}

.brand-text {
  display: flex;
  flex-direction: column;
  line-height: 1.2;
}

.brand-name {
  font: var(--md-sys-title-medium);
  letter-spacing: 0.15px;
}

.brand-subtitle {
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
}

.role-chip {
  display: inline-flex;
  align-items: center;
  height: 32px;
  padding: 0 16px;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
  font: var(--md-sys-label-large);
}

.login-btn {
  display: inline-flex;
  align-items: center;
  height: 36px;
  padding: 0 20px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  font: var(--md-sys-label-large);
  cursor: pointer;
  transition: box-shadow 0.2s;
}

.login-btn:hover {
  box-shadow: var(--md-sys-elevation-1);
}
</style>
