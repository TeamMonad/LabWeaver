<template>
  <RoleLayout
    icon="⚙️"
    title="管理员工作台"
    subtitle="资源审批、平台策略与审计日志"
    :tabs="tabs"
  >
    <RouterView />
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
