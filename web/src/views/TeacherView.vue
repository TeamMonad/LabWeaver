<template>
  <RoleLayout
    icon="👨‍🏫"
    title="教师工作台"
    subtitle="创建实验、审核 Agent 生成物、发布实验包"
    :tabs="tabs"
  >
    <RouterView />
  </RoleLayout>
</template>

<script setup lang="ts">
import RoleLayout from '@/components/layout/RoleLayout.vue'

const tabs = [
  { path: '/teacher', label: '实验列表' },
  { path: '/teacher/materials', label: '材料上传' },
  { path: '/teacher/approvals', label: '待审批' },
]
</script>
