<template>
  <RoleLayout
    icon="⚙️"
    title="管理员工作台"
    subtitle="资源审批、平台策略与审计日志"
    :tabs="tabs"
  >
    <div class="placeholder">
      <div class="placeholder-icon">🛡️</div>
      <h3 class="placeholder-title">资源审批</h3>
      <p class="placeholder-desc">审批 CPU/GPU 资源申请，查看平台配额、Kyverno 策略和审计日志。</p>
    </div>
  </RoleLayout>
</template>

<script setup lang="ts">
import RoleLayout from '@/components/layout/RoleLayout.vue'

const tabs = [
  { path: '/admin', label: '资源审批' },
  { path: '/admin/policies', label: '策略' },
  { path: '/admin/audit', label: '审计' },
]
</script>

<style scoped>
.placeholder {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  min-height: 260px;
  text-align: center;
  color: var(--md-sys-color-on-surface-variant);
}

.placeholder-icon {
  width: 64px;
  height: 64px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--md-sys-shape-large);
  background: var(--md-sys-color-surface-variant);
  font-size: 32px;
  margin-bottom: 16px;
}

.placeholder-title {
  font: var(--md-sys-title-large);
  color: var(--md-sys-color-on-surface);
  margin-bottom: 8px;
}

.placeholder-desc {
  font: var(--md-sys-body-medium);
  max-width: 520px;
  line-height: 1.6;
}
</style>
