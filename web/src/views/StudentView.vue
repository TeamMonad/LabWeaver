<template>
  <RoleLayout
    icon="person"
    title="学生工作台"
    subtitle="加入实验、启动环境、提交结果并查看反馈"
    :tabs="tabs"
  >
    <RouterView />
  </RoleLayout>
</template>

<script setup lang="ts">
import RoleLayout from '@/components/layout/RoleLayout.vue'

const tabs = [
  { path: '/student', label: '我的实验' },
  { path: '/student/environments', label: '环境入口' },
  { path: '/student/results', label: '评测结果' },
]
</script>
