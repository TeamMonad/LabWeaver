<template>
  <RoleLayout
    icon="person"
    title="学生工作台"
    subtitle="加入实验、启动环境并冻结不可变提交"
    :tabs="tabs"
  >
    <RouterView />
  </RoleLayout>
</template>

<script setup lang="ts">
import RoleLayout from '@/components/layout/RoleLayout.vue'

const tabs = [
  { path: '/student/labs', label: '我的实验' },
  { path: '/student/environments', label: '环境控制台' },
  { path: '/student/ssh-keys', label: 'SSH 公钥' },
  { path: '/student/results', label: '评测结果' },
]
</script>
