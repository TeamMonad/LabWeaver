<template>
  <RoleLayout
    icon="science"
    title="科研工作台"
    subtitle="后续 Sprint 的长期工作环境入口"
    :tabs="tabs"
  >
    <RouterView />
  </RoleLayout>
</template>

<script setup lang="ts">
import RoleLayout from '@/components/layout/RoleLayout.vue'

const tabs = [
  { path: '/researcher', label: '工作环境' },
]
</script>
