<template>
  <RoleLayout
    icon="admin_panel_settings"
    title="管理员工作台"
    subtitle="平台策略与高价值审计日志"
    :tabs="tabs"
  >
    <RouterView />
  </RoleLayout>
</template>

<script setup lang="ts">
import RoleLayout from '@/components/layout/RoleLayout.vue'

const tabs = [
  { path: '/admin/policies', label: '策略' },
  { path: '/admin/resource-approval', label: '资源审批' },
  { path: '/admin/audit', label: '审计' },
]
</script>
