<template>
  <RoleLayout
    icon="🔬"
    title="科研工作台"
    subtitle="长期工作环境、算力申请与软件配置"
    :tabs="tabs"
  >
    <RouterView />
  </RoleLayout>
</template>

<script setup lang="ts">
import RoleLayout from '@/components/layout/RoleLayout.vue'

const tabs = [
  { path: '/researcher', label: '工作环境' },
  { path: '/researcher/resources', label: '资源申请' },
  { path: '/researcher/config', label: '软件配置' },
]
</script>
