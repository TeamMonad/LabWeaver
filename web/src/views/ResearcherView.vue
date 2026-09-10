<template>
  <RoleLayout
    icon="science"
    title="科研工作台"
    subtitle="项目、Work 环境、Agent 配置与资源生命周期"
    :tabs="tabs"
  >
    <RouterView />
  </RoleLayout>
</template>

<script setup lang="ts">
import RoleLayout from '@/components/layout/RoleLayout.vue'

const tabs = [
  { path: '/researcher/workspaces', label: '项目与工作空间' },
  { path: '/researcher/environments', label: 'Work 环境' },
  { path: '/researcher/software', label: '软件配置' },
  { path: '/researcher/resources', label: '资源申请' },
]
</script>
