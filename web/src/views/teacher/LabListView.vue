<template>
  <MockList
    title="实验列表"
    description="教师发布的实验包将在这里展示，支持查看版本、状态和审批进度。"
    :columns="[
      { key: 'name', label: '实验名称' },
      { key: 'course', label: '所属课程' },
      { key: 'updatedAt', label: '更新时间' },
    ]"
    :rows="[
      { name: 'Linux 基础命令练习', course: '操作系统原理', updatedAt: '2026-07-12', status: '已发布', statusType: 'success' },
      { name: 'Python 数据结构实验', course: '程序设计基础', updatedAt: '2026-07-11', status: '待审批', statusType: 'warning' },
      { name: 'KubeVirt VM 启动预检', course: '云原生实验', updatedAt: '2026-07-10', status: '草稿', statusType: 'neutral' },
    ]"
  />
</template>

<script setup lang="ts">
import MockList from '@/components/common/MockList.vue'
</script>
